//! Global bindings, their builtin mirrors, and cache invalidation policy.

use std::borrow::Cow;

use crate::det_hash::IndexMap;
use crate::instr::Builtin;

use super::Val;
use super::caches::{GlobalLookupCacheEntry, GlobalLookupCacheSlot};
use super::object::{GcHeap, Markable, ObjectPtr};

pub(super) struct GlobalEnv {
    globals: IndexMap<String, Val>,
    builtins: [Val; Builtin::COUNT],
    version: u64,
}

impl GlobalEnv {
    pub(super) fn new() -> Self {
        Self {
            globals: IndexMap::default(),
            builtins: std::array::from_fn(|_| Val::Nil),
            version: 0,
        }
    }

    pub(super) fn reserve(&mut self, additional: usize) {
        self.globals.reserve(additional);
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&String, &Val)> {
        self.globals.iter()
    }

    /// Raw binding lookup, including tombstones, for tests that inspect the map.
    #[cfg(test)]
    pub(super) fn get(&self, name: &str) -> Option<&Val> {
        self.globals.get(name)
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.globals.is_empty()
    }

    #[inline(always)]
    pub(super) fn version(&self) -> u64 {
        self.version
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
                .map_or(Val::Nil, |(_, val)| *val);
        }
        Val::Nil
    }

    #[inline(always)]
    pub(super) fn read_cached(&self, cache: &GlobalLookupCacheSlot) -> Option<Val> {
        let entry = cache.get()?;
        if entry.globals_version != self.version {
            return None;
        }
        self.globals.get_index(entry.index).map(|(_, val)| *val)
    }

    // Existing entries keep their indices even when assigned nil. Removing
    // tombstones would invalidate caches and change iteration and snapshot order.
    #[inline(always)]
    fn write_binding(&mut self, name: Cow<'_, str>, val: Val, restore: bool) {
        if let Some(target) = self.globals.get_mut(name.as_ref()) {
            *target = val;
        } else if restore || !matches!(val, Val::Nil) {
            self.globals.insert(name.into_owned(), val);
        }
    }

    #[inline(always)]
    fn write_builtin_slot(&mut self, slot: u8, val: Val) {
        self.builtins[slot as usize] = val;
        // Builtin writes also invalidate conservatively guarded method caches.
        self.version = self.version.wrapping_add(1);
    }

    #[inline(always)]
    pub(super) fn write_builtin(&mut self, slot: u8, val: Val) {
        self.write_builtin_slot(slot, val);
        if let Some(builtin) = Builtin::from_u8(slot) {
            self.write_binding(Cow::Borrowed(builtin.name()), val, false);
        }
    }

    pub(super) fn write(&mut self, name: String, val: Val) {
        self.write_owned(name, val, false);
    }

    fn write_owned(&mut self, name: String, val: Val, restore: bool) {
        if let Some(slot) = Builtin::from_name(&name) {
            self.write_builtin_slot(slot as u8, val);
        }
        self.write_binding(Cow::Owned(name), val, restore);
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
            if let Some((_, target)) = self.globals.get_index_mut(index) {
                *target = val;
                if let Some(cache) = cache {
                    cache.set(GlobalLookupCacheEntry {
                        globals_version: self.version,
                        index,
                    });
                }
            }
        } else {
            // New entries remain cold until the next access finds their index.
            self.write_binding(Cow::Borrowed(name), val, false);
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
        let Some((_, target)) = self.globals.get_index_mut(entry.index) else {
            return false;
        };
        *target = val;
        true
    }

    #[cfg(feature = "snapshot")]
    pub(super) fn restore_binding(&mut self, name: String, val: Val) {
        // A saved nil is an existing tombstone, even if setup omitted its name.
        self.write_owned(name, val, true);
    }

    pub(super) fn restrict(&mut self, whitelist: &[&str]) -> Self {
        let mut restricted = Self::new();
        for name in whitelist {
            if let Some(slot) = Builtin::from_name(name) {
                restricted.builtins[slot as usize] = self.builtins[slot as usize];
            }
            if let Some(val) = self.globals.get(*name) {
                restricted.globals.insert((*name).to_owned(), *val);
            }
        }
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
        for (slot, value) in self.builtins.iter().enumerate() {
            if let Some(builtin) = Builtin::from_u8(slot as u8) {
                let mirrored = self
                    .globals
                    .get(builtin.name())
                    .copied()
                    .unwrap_or_default();
                // Lua equality rejects NaN and equates signed zeros; the
                // mirror invariant requires identical numeric bits instead.
                debug_assert!(match (*value, mirrored) {
                    (Val::Num(left), Val::Num(right)) => left.to_bits() == right.to_bits(),
                    (left, right) => left == right,
                });
            }
        }
        self.globals.mark_reachable(heap, worklist);
        self.builtins.mark_reachable(heap, worklist);
    }
}
