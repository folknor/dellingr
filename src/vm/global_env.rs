//! Global bindings, their builtin mirrors, and cache invalidation policy.
//!
//! A binding assigned nil stays in place as a tombstone, so cached indices and
//! the slot order stay put. Tombstones would otherwise accumulate forever: a
//! script writing computed names through `_G` grows the map with every name it
//! has ever used. Compaction removes them, but a load must still know which
//! deleted names to suppress, because load setup reinstalls its own globals
//! before the saved bindings are restored. That splits bindings in two:
//!
//! - **Pinned** bindings were written while the environment was initializing.
//!   Load setup may recreate them, so their tombstones are kept for the life of
//!   the environment.
//! - **Unpinned** bindings were first written after the host sealed the
//!   environment (`State::seal_globals`). Their tombstones are compacted.
//!
//! An environment initializes until sealed; an unsealed one pins every binding
//! and never compacts. Compaction runs only when a new name is inserted, and
//! only once unpinned tombstones make up half the map, so it costs amortised
//! O(1) per nil write and never moves a binding during a write to an existing
//! one. It is deterministic: it depends on the write history and the pin set,
//! and a sealed load reproduces both (see `restore`).

use std::borrow::Cow;

use crate::det_hash::IndexMap;
use crate::instr::Builtin;

use super::Val;
use super::caches::{GlobalLookupCacheEntry, GlobalLookupCacheSlot};
use super::object::{GcHeap, Markable, ObjectPtr};

#[derive(Clone, Copy)]
struct Binding {
    val: Val,
    pinned: bool,
}

impl Binding {
    #[inline(always)]
    fn is_compactable(self) -> bool {
        !self.pinned && matches!(self.val, Val::Nil)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// Every write pins its binding; nothing compacts.
    Initializing,
    /// `load_state` setup: initializing, and a `seal` is deferred to the
    /// saved phase so a shared setup helper that seals cannot change it.
    #[cfg(feature = "snapshot")]
    LoadSetup,
    /// Writes no longer pin; unpinned tombstones are compactable.
    Sealed,
}

pub(super) struct GlobalEnv {
    globals: IndexMap<String, Binding>,
    builtins: [Val; Builtin::COUNT],
    version: u64,
    phase: Phase,
    /// Unpinned nil bindings: the tombstones compaction may remove.
    compactable: usize,
}

impl GlobalEnv {
    pub(super) fn new() -> Self {
        Self {
            globals: IndexMap::default(),
            builtins: std::array::from_fn(|_| Val::Nil),
            version: 0,
            phase: Phase::Initializing,
            compactable: 0,
        }
    }

    pub(super) fn reserve(&mut self, additional: usize) {
        self.globals.reserve(additional);
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&String, Val)> {
        self.globals
            .iter()
            .map(|(name, binding)| (name, binding.val))
    }

    /// Raw binding lookup, including tombstones, for tests that inspect the map.
    #[cfg(test)]
    pub(super) fn get(&self, name: &str) -> Option<&Val> {
        self.globals.get(name).map(|binding| &binding.val)
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.globals.is_empty()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.globals.len()
    }

    pub(super) fn is_sealed(&self) -> bool {
        self.phase == Phase::Sealed
    }

    /// Stop pinning. One-way; deferred while load setup runs.
    pub(super) fn seal(&mut self) {
        if self.phase == Phase::Initializing {
            self.phase = Phase::Sealed;
        }
    }

    #[cfg(feature = "snapshot")]
    pub(super) fn begin_load_setup(&mut self) {
        self.phase = Phase::LoadSetup;
    }

    #[inline(always)]
    pub(super) fn builtin(&self, slot: u8) -> Val {
        self.builtins[slot as usize]
    }

    #[inline(always)]
    pub(super) fn read(&self, name: &str, cache: Option<&GlobalLookupCacheSlot>) -> Val {
        if let Some(slot) = Builtin::from_name(name) {
            return self.builtin(slot as u8);
        }
        if let Some(index) = self.globals.get_index_of(name) {
            if let Some(cache) = cache {
                cache.set(GlobalLookupCacheEntry {
                    globals_version: self.version,
                    index,
                });
            }
            return self
                .globals
                .get_index(index)
                .map_or(Val::Nil, |(_, binding)| binding.val);
        }
        Val::Nil
    }

    #[inline(always)]
    pub(super) fn read_cached(&self, cache: &GlobalLookupCacheSlot) -> Option<Val> {
        let entry = cache.get()?;
        if entry.globals_version != self.version {
            return None;
        }
        self.globals
            .get_index(entry.index)
            .map(|(_, binding)| binding.val)
    }

    /// Overwrite the binding at `index`, keeping its slot, and account for a
    /// tombstone appearing or disappearing. False if `index` is out of range.
    #[inline(always)]
    fn write_at(&mut self, index: usize, val: Val) -> bool {
        let pin = self.phase != Phase::Sealed;
        let Some((_, binding)) = self.globals.get_index_mut(index) else {
            return false;
        };
        let was_compactable = usize::from(binding.is_compactable());
        binding.val = val;
        binding.pinned |= pin;
        // Branch-free: the count includes this binding when `was` is 1, so
        // adding before subtracting cannot underflow.
        self.compactable =
            self.compactable + usize::from(binding.is_compactable()) - was_compactable;
        true
    }

    /// Append a new non-nil binding, compacting first when unpinned
    /// tombstones make up at least half the map.
    fn insert_new(&mut self, name: String, val: Val) {
        debug_assert!(!matches!(val, Val::Nil));
        if self.compactable > 0 && self.compactable * 2 >= self.globals.len() {
            self.compact();
        }
        let pinned = self.phase != Phase::Sealed;
        self.globals.insert(name, Binding { val, pinned });
    }

    fn count_compactable(&self) -> usize {
        self.globals
            .values()
            .filter(|binding| binding.is_compactable())
            .count()
    }

    /// Drop unpinned tombstones, preserving the order of what remains. Indices
    /// move, so cached ones are invalidated.
    fn compact(&mut self) {
        self.globals.retain(|_, binding| !binding.is_compactable());
        self.compactable = 0;
        self.version = self.version.wrapping_add(1);
    }

    #[inline(always)]
    fn write_binding(&mut self, name: Cow<'_, str>, val: Val) {
        if let Some(index) = self.globals.get_index_of(name.as_ref()) {
            self.write_at(index, val);
        } else if !matches!(val, Val::Nil) {
            self.insert_new(name.into_owned(), val);
        }
    }

    #[inline(always)]
    fn write_builtin_slot(&mut self, slot: u8, val: Val) {
        self.builtins[slot as usize] = val;
        // Builtin reads bypass global slot caches. Updating an existing
        // binding leaves all cached indices valid; insertion/compaction and
        // environment replacement own any structural invalidation needed.
    }

    #[inline(always)]
    pub(super) fn write_builtin(&mut self, slot: u8, val: Val) {
        self.write_builtin_slot(slot, val);
        if let Some(builtin) = Builtin::from_u8(slot) {
            self.write_binding(Cow::Borrowed(builtin.name()), val);
        }
    }

    pub(super) fn write(&mut self, name: String, val: Val) {
        if let Some(slot) = Builtin::from_name(&name) {
            self.write_builtin_slot(slot as u8, val);
        }
        self.write_binding(Cow::Owned(name), val);
    }

    #[inline(always)]
    pub(super) fn write_named(
        &mut self,
        name: &str,
        val: Val,
        cache: Option<&GlobalLookupCacheSlot>,
    ) {
        if let Some(slot) = Builtin::from_name(name) {
            self.write_builtin(slot as u8, val);
        } else if let Some(index) = self.globals.get_index_of(name) {
            self.write_at(index, val);
            if let Some(cache) = cache {
                cache.set(GlobalLookupCacheEntry {
                    globals_version: self.version,
                    index,
                });
            }
        } else if !matches!(val, Val::Nil) {
            // New entries remain cold until the next access finds their index.
            self.insert_new(name.to_owned(), val);
        }
    }

    #[inline(always)]
    pub(super) fn write_cached(&mut self, cache: &GlobalLookupCacheSlot, val: Val) -> bool {
        let Some(entry) = cache.get() else {
            return false;
        };
        if entry.globals_version != self.version {
            return false;
        }
        self.write_at(entry.index, val)
    }

    /// Replace the setup environment with the saved bindings, in saved slot
    /// order and tombstones included, then append the bindings setup created
    /// that the save does not mention (setup changed since the save).
    ///
    /// A sealed save loads sealed, pinning exactly the names setup retained:
    /// with the same initialization history that is the pin set the saving
    /// environment had, so both continue and compact identically. An unsealed
    /// save loads unsealed with every binding pinned, as the saving one was.
    #[cfg(feature = "snapshot")]
    pub(super) fn restore(&mut self, saved: Vec<(String, Val)>, sealed: bool) {
        let setup = std::mem::take(&mut self.globals);
        let mut globals = crate::det_hash::map_with_capacity(saved.len() + setup.len());
        for (name, val) in saved {
            let pinned = !sealed || setup.contains_key(&name);
            globals.insert(name, Binding { val, pinned });
        }
        for (name, binding) in setup {
            if !globals.contains_key(&name) {
                globals.insert(name, binding);
            }
        }
        self.globals = globals;
        self.phase = if sealed {
            Phase::Sealed
        } else {
            Phase::Initializing
        };
        self.compactable = self.count_compactable();
        for (slot, mirror) in self.builtins.iter_mut().enumerate() {
            if let Some(builtin) = Builtin::from_u8(slot as u8) {
                *mirror = self
                    .globals
                    .get(builtin.name())
                    .map_or(Val::Nil, |binding| binding.val);
            }
        }
        // Setup may have run Lua and warmed caches against its own indices.
        self.version = self.version.wrapping_add(1);
    }

    pub(super) fn restrict(&mut self, whitelist: &[&str]) -> Self {
        let mut restricted = Self::new();
        restricted.phase = Phase::Sealed;
        for name in whitelist {
            if let Some(slot) = Builtin::from_name(name) {
                restricted.builtins[slot as usize] = self.builtins[slot as usize];
            }
            if let Some(binding) = self.globals.get(*name) {
                restricted.globals.insert((*name).to_owned(), *binding);
            }
        }
        // Counted afterwards: a repeated whitelist name replaces one binding.
        restricted.compactable = restricted.count_compactable();
        self.replace(restricted)
    }

    pub(super) fn replace(&mut self, mut replacement: Self) -> Self {
        // Advance from the active environment, including writes made while
        // restricted, rather than restoring the suspended version.
        replacement.version = self.version.wrapping_add(1);
        std::mem::replace(self, replacement)
    }
}

impl Markable for GlobalEnv {
    fn mark_reachable(&self, heap: &GcHeap, worklist: &mut Vec<ObjectPtr>) {
        #[cfg(debug_assertions)]
        {
            for (slot, value) in self.builtins.iter().enumerate() {
                if let Some(builtin) = Builtin::from_u8(slot as u8) {
                    let mirrored = self
                        .globals
                        .get(builtin.name())
                        .map_or(Val::Nil, |binding| binding.val);
                    // Lua equality rejects NaN and equates signed zeros; the
                    // mirror invariant requires identical numeric bits instead.
                    debug_assert!(match (*value, mirrored) {
                        (Val::Num(left), Val::Num(right)) => left.to_bits() == right.to_bits(),
                        (left, right) => left == right,
                    });
                }
            }
            debug_assert_eq!(self.compactable, self.count_compactable());
        }
        for binding in self.globals.values() {
            binding.val.mark_reachable(heap, worklist);
        }
        self.builtins.mark_reachable(heap, worklist);
    }
}

#[cfg(test)]
mod tests {
    use super::GlobalEnv;
    use crate::instr::Builtin;
    use crate::vm::Val;
    use crate::vm::caches::GlobalLookupCacheSlot;

    #[test]
    fn builtin_rebind_keeps_global_slot_cache_valid() {
        let mut env = GlobalEnv::new();
        env.write("target".to_owned(), Val::Num(7.0));
        env.write_builtin(Builtin::Print as u8, Val::Nil);
        let cache = GlobalLookupCacheSlot::default();
        env.read("target", Some(&cache));
        let version = env.version;
        env.write_builtin(Builtin::Print as u8, Val::Num(1.0));
        assert_eq!(env.version, version);
        assert_eq!(env.read_cached(&cache), Some(Val::Num(7.0)));
        env.write("print".to_owned(), Val::Nil);
        assert_eq!(env.version, version);
        assert_eq!(env.read_cached(&cache), Some(Val::Num(7.0)));
    }
}
