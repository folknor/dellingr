//! The global environment: one heap table, which is also `_G`.
//!
//! Every global binding is an entry of that table, so bytecode globals, `_G`
//! field access and the raw primitives (`rawget`, `rawset`, `next`, `pairs`,
//! the table library, the host's raw table API) all read and write the same
//! storage. The table's identity never changes for the life of the State:
//! `with_restricted_env` swaps its contents, not the object, so any reference
//! to `_G` always sees the environment in force.
//!
//! Lookups are cached by the table's shape version. A cached slot index stays
//! valid while the version is unchanged, and a cached absence also needs the
//! hash slot count unchanged (see `Table::hash_slot_count`). The builtin names
//! (`Builtin`) are interned once and read through a mirror validated by the
//! heap's mutation epoch for this table (`builtin_mirror`), so the builtin
//! opcodes do not reach the table at all while no builtin has changed.
//!
//! Initialization and sealing: until the host seals the environment
//! (`State::seal_globals`), the table retains every tombstone
//! (`SlotPolicy::retain_dead`), so every name initialization wrote keeps a slot.
//! Sealing ends that, and under the `snapshot` feature records those names as
//! the pin list: a load must keep a pinned name deleted even after its
//! tombstone compacts away, because load setup reinstalls it (see
//! docs/save-state.md).

use std::cell::Cell;

use crate::instr::Builtin;

use super::Val;
use super::caches::{GlobalLookupCacheEntry, GlobalLookupCacheSlot, GlobalLookupIndex};
use super::object::{GcHeap, Markable, ObjectPtr, StringPtr};
use super::table::{SlotPolicy, Table};

/// Hash slots the global table starts with: past inline storage, because
/// only map storage carries a slot policy.
const INITIAL_GLOBAL_SLOTS: usize = crate::limits::tuning::TABLE_INLINE_CAPACITY + 1;

/// The global table's slot policy in `phase`. A global keeps its slot (and
/// its place in `pairs(_G)`) however often it is cleared and set, as a
/// binding always has; tombstones are retained until the environment seals.
fn slot_policy(phase: Phase) -> SlotPolicy {
    SlotPolicy {
        revive_in_place: true,
        retain_dead: phase != Phase::Sealed,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// The table retains every tombstone.
    Initializing,
    /// `load_state` setup: initializing, and a `seal` is deferred to the
    /// saved phase so a shared setup helper that seals cannot change it.
    #[cfg(feature = "snapshot")]
    LoadSetup,
    /// Tombstones compact as in any table.
    Sealed,
}

pub(super) struct GlobalEnv {
    /// The global table, which `_G` names.
    table: ObjectPtr,
    /// The builtin names, interned and rooted for the State's life.
    builtin_names: [StringPtr; Builtin::COUNT],
    /// One lookup cache per builtin, shared by its reads and writes.
    builtin_caches: [GlobalLookupCacheSlot; Builtin::COUNT],
    /// Each builtin's value as last read, with the global table's mutation
    /// epoch (`GcHeap::watched_epoch`) at that read. While the epoch is
    /// unchanged no builtin's slot has changed, so the value is current
    /// without reaching the table at all. Every mutable access to the table
    /// moves the epoch, except a cached store to a slot whose key is not a
    /// builtin name (`write_cached`), which cannot change one; the next read
    /// after a move refreshes the entry.
    builtin_mirror: [Cell<(u64, Val)>; Builtin::COUNT],
    phase: Phase,
    /// Every string key that had a slot when the environment was sealed (or
    /// that load setup contributed to a sealed load), in slot order. Empty
    /// while initializing: the retained tombstones remember those names then.
    #[cfg(feature = "snapshot")]
    pins: Vec<StringPtr>,
}

/// An environment suspended by `with_restricted_env`: the global table's
/// contents moved out of the heap object, and the phase they had.
pub(super) struct SuspendedEnv {
    contents: Table,
    phase: Phase,
}

impl SuspendedEnv {
    /// A suspended environment that came from no State, for tests of the
    /// bookkeeping that holds one.
    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn detached() -> Self {
        Self {
            contents: Table::default(),
            phase: Phase::Sealed,
        }
    }
}

impl Markable for SuspendedEnv {
    fn mark_reachable(&self, heap: &GcHeap, worklist: &mut Vec<ObjectPtr>) {
        // Detached contents, not the heap object: that holds the restricted
        // environment while this is suspended.
        self.contents.mark_values(heap, worklist);
    }
}

impl GlobalEnv {
    /// Build the empty environment while the heap is being built: nothing
    /// else exists yet, so allocating without collection is safe.
    pub(super) fn new(heap: &mut GcHeap) -> Self {
        let table = heap
            .no_gc_allocator()
            .alloc_table_with_capacity(INITIAL_GLOBAL_SLOTS);
        heap.as_table(table)
            .expect("just allocated as a table")
            .set_slot_policy(slot_policy(Phase::Initializing));
        heap.watch(table);
        let builtin_names = std::array::from_fn(|slot| {
            let builtin =
                Builtin::from_u8(slot as u8).expect("every slot below COUNT names a builtin");
            heap.no_gc_allocator()
                .alloc_string(builtin.name().as_bytes())
        });
        // No epoch matches before the first read: `watch` moved it past 0.
        let stale = heap.watched_epoch().wrapping_sub(1);
        Self {
            table,
            builtin_names,
            builtin_caches: std::array::from_fn(|_| GlobalLookupCacheSlot::default()),
            builtin_mirror: std::array::from_fn(|_| Cell::new((stale, Val::Nil))),
            phase: Phase::Initializing,
            #[cfg(feature = "snapshot")]
            pins: Vec::new(),
        }
    }

    /// The global table, which `_G` names.
    #[inline(always)]
    pub(super) fn table(&self) -> ObjectPtr {
        self.table
    }

    #[inline(always)]
    fn contents<'h>(&self, heap: &'h GcHeap) -> &'h Table {
        heap.as_table_ref(self.table)
            .expect("the global table lives as long as the State")
    }

    #[inline(always)]
    fn contents_mut<'h>(&self, heap: &'h mut GcHeap) -> &'h mut Table {
        heap.as_table(self.table)
            .expect("the global table lives as long as the State")
    }

    pub(super) fn reserve(&self, heap: &mut GcHeap, additional: usize) {
        self.contents_mut(heap).reserve_hash(additional);
    }

    pub(super) fn is_sealed(&self) -> bool {
        self.phase == Phase::Sealed
    }

    /// Stop retaining tombstones and, under `snapshot`, record the pins.
    /// One-way; deferred while load setup runs.
    pub(super) fn seal(&mut self, heap: &mut GcHeap) {
        if self.phase != Phase::Initializing {
            return;
        }
        self.phase = Phase::Sealed;
        let table = self.contents_mut(heap);
        #[cfg(feature = "snapshot")]
        {
            self.pins = string_slot_keys(table).collect();
        }
        table.set_slot_policy(slot_policy(Phase::Sealed));
    }

    #[cfg(feature = "snapshot")]
    pub(super) fn begin_load_setup(&mut self) {
        self.phase = Phase::LoadSetup;
    }

    /// The interned key for builtin `slot`.
    #[inline(always)]
    pub(super) fn builtin_key(&self, slot: u8) -> Val {
        Val::Str(self.builtin_names[usize::from(slot)])
    }

    /// Whether `key` is a builtin name. For cache fills, off the hot path.
    fn is_builtin_key(&self, key: Val) -> bool {
        matches!(key, Val::Str(ptr) if self.builtin_names.contains(&ptr))
    }

    /// The value of global `key` if `cache` still answers for it.
    #[inline(always)]
    pub(super) fn read_cached(&self, heap: &GcHeap, cache: &GlobalLookupCacheSlot) -> Option<Val> {
        let entry = cache.get()?;
        let table = self.contents(heap);
        if entry.version != table.version() {
            return None;
        }
        match entry.index {
            GlobalLookupIndex::Present(index) => table.hash_slot_value(index),
            GlobalLookupIndex::Absent(count) => {
                (count == table.hash_slot_count()).then_some(Val::Nil)
            }
        }
    }

    /// The value of global `key`, a string, filling `cache`. A cleared
    /// global keeps its slot (`SlotPolicy::revive_in_place`), so its entry
    /// caches the slot and sees the global come back; only a name with no
    /// slot at all is cached as absent.
    #[inline(never)]
    pub(super) fn read(
        &self,
        heap: &GcHeap,
        key: Val,
        cache: Option<&GlobalLookupCacheSlot>,
    ) -> Val {
        let table = self.contents(heap);
        let (index, value) = match table.hash_slot_of(&key) {
            Some((index, value)) => (GlobalLookupIndex::Present(index), value),
            None => (GlobalLookupIndex::Absent(table.hash_slot_count()), Val::Nil),
        };
        if let Some(cache) = cache {
            cache.set(GlobalLookupCacheEntry {
                version: table.version(),
                index,
                builtin: self.is_builtin_key(key),
            });
        }
        value
    }

    /// Store through `cache` if it still names the key's slot. False when
    /// the caller must take the keyed path: no entry, a stale one, or an
    /// absence (never a writable index).
    #[inline(always)]
    pub(super) fn write_cached(
        &self,
        heap: &mut GcHeap,
        cache: &GlobalLookupCacheSlot,
        val: Val,
    ) -> bool {
        let Some(entry) = cache.get() else {
            return false;
        };
        // A store to a non-builtin slot cannot change a builtin, so only a
        // builtin's moves the mirror's epoch (`builtin_mirror`).
        if entry.builtin {
            heap.mark_watched_changed();
        }
        let table = heap
            .watched_table_unmarked()
            .expect("the global table lives as long as the State");
        if entry.version != table.version() {
            return false;
        }
        match entry.index {
            // A live store over a live value, the steady state, needs no
            // tombstone accounting.
            GlobalLookupIndex::Present(index) => {
                if matches!(val, Val::Nil) || !table.set_at_index(index, val) {
                    table.write_slot(index, val);
                }
                true
            }
            GlobalLookupIndex::Absent(_) => false,
        }
    }

    /// Store global `key`, a string. A binding with a slot (live or cleared)
    /// refills `cache`; a new one stays cold until the next access finds its
    /// slot.
    #[inline(never)]
    pub(super) fn write(
        &self,
        heap: &mut GcHeap,
        key: Val,
        val: Val,
        cache: Option<&GlobalLookupCacheSlot>,
    ) {
        let table = self.contents_mut(heap);
        if let Some((index, _)) = table.hash_slot_of(&key) {
            table.write_slot(index, val);
            if let Some(cache) = cache {
                cache.set(GlobalLookupCacheEntry {
                    version: table.version(),
                    index: GlobalLookupIndex::Present(index),
                    builtin: self.is_builtin_key(key),
                });
            }
        } else if !matches!(val, Val::Nil) {
            table
                .insert(key, val)
                .expect("a string key lives in the hash part, whose insertion cannot fail");
        }
    }

    #[inline(always)]
    pub(super) fn read_builtin(&self, heap: &GcHeap, slot: u8) -> Val {
        let (epoch, val) = self.builtin_mirror[usize::from(slot)].get();
        if epoch == heap.watched_epoch() {
            return val;
        }
        self.refresh_builtin(heap, slot)
    }

    /// Read builtin `slot` from the table and record it in the mirror.
    #[inline(never)]
    fn refresh_builtin(&self, heap: &GcHeap, slot: u8) -> Val {
        let cache = &self.builtin_caches[usize::from(slot)];
        let val = match self.read_cached(heap, cache) {
            Some(val) => val,
            None => self.read(heap, self.builtin_key(slot), Some(cache)),
        };
        self.builtin_mirror[usize::from(slot)].set((heap.watched_epoch(), val));
        val
    }

    #[inline(always)]
    pub(super) fn write_builtin(&self, heap: &mut GcHeap, slot: u8, val: Val) {
        let cache = &self.builtin_caches[usize::from(slot)];
        if !self.write_cached(heap, cache, val) {
            self.write(heap, self.builtin_key(slot), val, Some(cache));
        }
    }

    /// The value of the global named `name`, for the host. A name nothing has
    /// interned cannot be bound, so the lookup never allocates.
    pub(super) fn read_name(&self, heap: &GcHeap, name: &str) -> Val {
        if let Some(builtin) = Builtin::from_name(name) {
            return self.read_builtin(heap, builtin.to_u8());
        }
        heap.find_string(name.as_bytes())
            .map_or(Val::Nil, |key| self.read(heap, Val::Str(key), None))
    }

    /// Store a global under an interned `key`, through the builtin cache when
    /// the key is a builtin name.
    pub(super) fn write_key(&self, heap: &mut GcHeap, key: StringPtr, val: Val) {
        match self.builtin_names.iter().position(|name| *name == key) {
            Some(slot) => self.write_builtin(heap, slot as u8, val),
            None => self.write(heap, Val::Str(key), val, None),
        }
    }

    /// Live global bindings in table order, for diagnostics that look a
    /// value up by the name it is bound to. Only string keys name globals.
    pub(super) fn live_string_bindings<'h>(
        &self,
        heap: &'h GcHeap,
    ) -> impl Iterator<Item = (StringPtr, Val)> + 'h {
        self.contents(heap)
            .live_hash_entries()
            .filter_map(|(_, key, value)| match key {
                Val::Str(ptr) => Some((ptr, value)),
                _ => None,
            })
    }

    /// Suspend this environment: the whitelisted globals become the table's
    /// whole contents, sealed, and the previous contents are returned.
    pub(super) fn restrict(&mut self, heap: &mut GcHeap, whitelist: &[&str]) -> SuspendedEnv {
        let outer = self.contents(heap);
        let mut restricted = Table::with_capacity(INITIAL_GLOBAL_SLOTS.max(whitelist.len()));
        restricted.set_slot_policy(slot_policy(Phase::Sealed));
        for name in whitelist {
            let Some(key) = heap.find_string(name.as_bytes()) else {
                continue;
            };
            let key = Val::Str(key);
            let value = outer.get(&key);
            if !matches!(value, Val::Nil) {
                restricted
                    .insert(key, value)
                    .expect("a string key lives in the hash part, whose insertion cannot fail");
            }
        }
        restricted.set_metatable(outer.get_metatable());
        let contents = self.swap_contents(heap, restricted);
        let phase = std::mem::replace(&mut self.phase, Phase::Sealed);
        SuspendedEnv { contents, phase }
    }

    /// Resume a suspended environment, discarding the restricted one.
    pub(super) fn resume(&mut self, heap: &mut GcHeap, suspended: SuspendedEnv) {
        self.swap_contents(heap, suspended.contents);
        self.phase = suspended.phase;
    }

    /// Put `incoming` into the global table and return what it held. The
    /// version moves past both, so every cache entry naming the table (in any
    /// cache family) misses or revalidates by key.
    fn swap_contents(&self, heap: &mut GcHeap, mut incoming: Table) -> Table {
        let table = self.contents_mut(heap);
        std::mem::swap(table, &mut incoming);
        table.advance_version_past(incoming.version());
        incoming
    }

    /// Raw slot lookup, tombstones included, for tests that inspect the table.
    #[cfg(test)]
    pub(super) fn get(&self, heap: &GcHeap, name: &str) -> Option<Val> {
        let key = Val::Str(heap.find_string(name.as_bytes())?);
        self.contents(heap)
            .hash_slot_of(&key)
            .map(|(_, value)| value)
    }

    /// Whether the table has no slots at all, for tests.
    #[cfg(test)]
    pub(super) fn is_empty(&self, heap: &GcHeap) -> bool {
        self.len(heap) == 0
    }

    /// Occupied hash slots, tombstones included, for tests.
    #[cfg(test)]
    pub(super) fn len(&self, heap: &GcHeap) -> usize {
        self.contents(heap).hash_slot_count()
    }

    /// Live binding names in table order, for tests.
    #[cfg(test)]
    pub(super) fn names(&self, heap: &GcHeap) -> Vec<String> {
        self.live_string_bindings(heap)
            .map(|(name, _)| String::from_utf8_lossy(heap.get_string(name)).into_owned())
            .collect()
    }
}

/// The string keys of every hash slot, live or dead, in slot order.
#[cfg(feature = "snapshot")]
fn string_slot_keys(table: &Table) -> impl Iterator<Item = StringPtr> + '_ {
    table
        .snapshot_hash_slots()
        .into_iter()
        .filter_map(|(key, _)| match key {
            Val::Str(ptr) => Some(ptr),
            _ => None,
        })
}

#[cfg(feature = "snapshot")]
impl GlobalEnv {
    /// The pin list, for the save.
    pub(super) fn pins(&self) -> &[StringPtr] {
        &self.pins
    }

    /// Replace load setup's environment with a saved one.
    ///
    /// `entries` is the saved table in `Table::snapshot_entries` form, which
    /// is authoritative for its array part and every key it holds, tombstones
    /// included. Setup contributes only string-keyed slots (live or dead) the
    /// save mentions nowhere, neither as a slot nor as a pin; they follow the
    /// saved slots in setup order. A sealed load pins the saved pins followed
    /// by every other string key setup gave a slot, so a deletion inherited
    /// from the save survives a setup that no longer installs the name.
    pub(super) fn restore(
        &mut self,
        heap: &mut GcHeap,
        (array_len, array_slots, mut entries): (usize, usize, Vec<(Val, Val)>),
        metatable: Option<ObjectPtr>,
        sealed: bool,
        saved_pins: Vec<StringPtr>,
    ) -> Result<(), super::table::TableError> {
        use super::table::TableKey;
        use crate::det_hash::{FixedState, IndexSet};

        let table = self.contents_mut(heap);
        let setup: Vec<StringPtr> = string_slot_keys(table).collect();
        let setup_slots = table.snapshot_hash_slots();
        let mut mentioned: IndexSet<TableKey> = IndexSet::with_capacity_and_hasher(
            entries.len() + saved_pins.len(),
            FixedState::default(),
        );
        for (key, _) in &entries {
            if !matches!(key, Val::Nil) {
                mentioned.insert(TableKey::from_prevalidated(*key));
            }
        }
        for pin in &saved_pins {
            mentioned.insert(TableKey::from_prevalidated(Val::Str(*pin)));
        }
        entries.extend(setup_slots.into_iter().filter(|(key, _)| {
            matches!(key, Val::Str(_)) && !mentioned.contains(&TableKey::from_prevalidated(*key))
        }));
        table.rebuild_from_snapshot(array_len, array_slots, entries)?;
        table.set_metatable(metatable);
        table.set_slot_policy(slot_policy(if sealed {
            Phase::Sealed
        } else {
            Phase::Initializing
        }));
        // Distinct saved ids can intern to one string; keep the first.
        self.pins = if sealed {
            let mut pins: IndexSet<StringPtr> = IndexSet::with_capacity_and_hasher(
                saved_pins.len() + setup.len(),
                FixedState::default(),
            );
            pins.extend(saved_pins);
            pins.extend(setup);
            pins.into_iter().collect()
        } else {
            Vec::new()
        };
        self.phase = if sealed {
            Phase::Sealed
        } else {
            Phase::Initializing
        };
        Ok(())
    }
}

impl Markable for GlobalEnv {
    fn mark_reachable(&self, heap: &GcHeap, worklist: &mut Vec<ObjectPtr>) {
        // A current mirror entry must hold exactly the table's value: an
        // epoch that failed to move on some write path shows up here.
        #[cfg(debug_assertions)]
        for slot in 0..Builtin::COUNT {
            let (epoch, mirrored) = self.builtin_mirror[slot].get();
            if epoch == heap.watched_epoch() {
                let actual = self
                    .contents(heap)
                    .hash_slot_of(&self.builtin_key(slot as u8))
                    .map_or(Val::Nil, |(_, value)| value);
                // Lua equality rejects NaN and equates signed zeros; the
                // mirror must hold identical numeric bits instead.
                debug_assert!(match (mirrored, actual) {
                    (Val::Num(left), Val::Num(right)) => left.to_bits() == right.to_bits(),
                    (left, right) => left == right,
                });
            }
        }
        heap.mark(self.table, worklist);
        for name in &self.builtin_names {
            heap.mark_string(*name);
        }
        #[cfg(feature = "snapshot")]
        for pin in &self.pins {
            heap.mark_string(*pin);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::State;
    use crate::instr::Builtin;
    use crate::vm::Val;
    use crate::vm::caches::GlobalLookupCacheSlot;

    fn key(state: &mut State, name: &str) -> Val {
        state.alloc_string(name).expect("short name")
    }

    #[test]
    fn builtin_rebind_keeps_global_slot_cache_valid() {
        let mut state = State::raw();
        let target = key(&mut state, "target");
        let env = &state.global_env;
        env.write(&mut state.heap, target, Val::Num(7.0), None);
        env.write_builtin(&mut state.heap, Builtin::Print as u8, Val::Num(0.0));
        let cache = GlobalLookupCacheSlot::default();
        env.read(&state.heap, target, Some(&cache));
        env.write_builtin(&mut state.heap, Builtin::Print as u8, Val::Num(1.0));
        assert_eq!(env.read_cached(&state.heap, &cache), Some(Val::Num(7.0)));
        env.write_builtin(&mut state.heap, Builtin::Print as u8, Val::Nil);
        assert_eq!(env.read_cached(&state.heap, &cache), Some(Val::Num(7.0)));
    }

    #[test]
    fn absent_global_cache_survives_value_writes_but_not_definition() {
        let mut state = State::raw();
        let existing = key(&mut state, "existing");
        let optional = key(&mut state, "optional");
        let env = &state.global_env;
        env.write(&mut state.heap, existing, Val::Num(1.0), None);
        let cache = GlobalLookupCacheSlot::default();
        assert_eq!(env.read(&state.heap, optional, Some(&cache)), Val::Nil);
        assert_eq!(env.read_cached(&state.heap, &cache), Some(Val::Nil));
        env.write(&mut state.heap, optional, Val::Nil, None);
        env.write(&mut state.heap, existing, Val::Num(2.0), None);
        assert_eq!(env.read_cached(&state.heap, &cache), Some(Val::Nil));
        assert!(!env.write_cached(&mut state.heap, &cache, Val::Num(3.0)));
        env.write(&mut state.heap, optional, Val::Num(3.0), Some(&cache));
        assert_eq!(env.read_cached(&state.heap, &cache), None);
        assert_eq!(env.read(&state.heap, optional, Some(&cache)), Val::Num(3.0));
        assert_eq!(env.read_cached(&state.heap, &cache), Some(Val::Num(3.0)));
    }

    #[test]
    fn a_cleared_global_keeps_its_slot_and_its_cache() {
        for seal in [false, true] {
            let mut state = State::raw();
            if seal {
                state.seal_globals();
            }
            let removed = key(&mut state, "removed");
            let env = &state.global_env;
            env.write(&mut state.heap, removed, Val::Num(1.0), None);
            env.write(&mut state.heap, removed, Val::Nil, None);
            let slots = env.len(&state.heap);
            let cache = GlobalLookupCacheSlot::default();
            assert_eq!(env.read(&state.heap, removed, Some(&cache)), Val::Nil);
            assert_eq!(env.read_cached(&state.heap, &cache), Some(Val::Nil));
            // Revived through another path, in place: the cached slot sees it.
            env.write(&mut state.heap, removed, Val::Num(5.0), None);
            assert_eq!(env.read_cached(&state.heap, &cache), Some(Val::Num(5.0)));
            assert!(env.write_cached(&mut state.heap, &cache, Val::Nil));
            assert!(env.write_cached(&mut state.heap, &cache, Val::Num(6.0)));
            assert_eq!(env.read_cached(&state.heap, &cache), Some(Val::Num(6.0)));
            assert_eq!(env.len(&state.heap), slots);
        }
    }

    #[test]
    fn global_caches_invalidate_on_both_environment_swaps() {
        let mut state = State::raw();
        let optional = key(&mut state, "optional");
        state
            .global_env
            .write(&mut state.heap, optional, Val::Num(6.0), None);
        let cache = GlobalLookupCacheSlot::default();
        let saved = state.global_env.restrict(&mut state.heap, &[]);
        state.global_env.read(&state.heap, optional, Some(&cache));
        assert_eq!(
            state.global_env.read_cached(&state.heap, &cache),
            Some(Val::Nil)
        );
        state.global_env.resume(&mut state.heap, saved);
        assert_eq!(state.global_env.read_cached(&state.heap, &cache), None);
        assert_eq!(
            state.global_env.read(&state.heap, optional, Some(&cache)),
            Val::Num(6.0)
        );
        let saved = state.global_env.restrict(&mut state.heap, &[]);
        assert_eq!(state.global_env.read_cached(&state.heap, &cache), None);
        state.global_env.resume(&mut state.heap, saved);
        assert_eq!(state.global_env.read_cached(&state.heap, &cache), None);
    }
}
