//! The map form of a table's hash part: an insertion-ordered slot vector
//! beside a lookup index that the table owns.
//!
//! Slots are `(key, value)` pairs in physical order, which is `pairs` order,
//! snapshot order and what slot tokens name. A slot is one of three things:
//!
//! - live: a real key with a non-nil value;
//! - dead (a tombstone): a real key with a nil value, kept so a cleared key
//!   stays a valid `next` control and so a revived global keeps its place;
//! - vacant: key and value both nil, the old position of a key that was
//!   re-added at the back. Nothing can look a vacant slot up; it only holds
//!   its position until compaction.
//!
//! The index holds the position of every real key, live or dead, exactly
//! once, and never a vacant slot. That is what makes a re-add that does not compact one probe: the
//! lookup that finds the dead key holds its index entry (`SlotMut`), and
//! the entry is retargeted to the appended slot without a second probe, where an
//! `IndexMap` had to rename the old slot to a synthetic key (erasing one
//! index entry and inserting another) and then insert the real key again.
//!
//! The index is only ever probed, never iterated, so its layout cannot reach
//! program-visible order. Hashes are not stored: rehashing recomputes them
//! from the slot keys, which hash as a pointer or number bits.

use std::hash::BuildHasher;
use std::sync::Arc;

use hashbrown::HashTable;

use super::{ResidentBytes, TableLookupKey, Val};
use crate::det_hash::FixedState;
use crate::limits::tuning::RESIDENT_BYTES_PER_HASH_ENTRY;

/// The seed is fixed on purpose: probe counts must not vary per process.
/// Numeric keys hash by their bits, so a script or a forged save can pick
/// keys that cluster and make inserts slow. That costs time only: equality
/// decides lookups and the slot vector decides order. Snapshot load accepts
/// it because loading is a trusted-input operation (docs/save-state.md).
#[inline(always)]
fn hash_key(key: Val) -> u64 {
    FixedState::default().hash_one(TableLookupKey(key))
}

#[derive(Debug, Default)]
pub(super) struct HashSlots {
    entries: Vec<(Val, Val)>,
    index: Index,
}

/// The lookup index, owned or shared copy-on-write.
///
/// Constructor-template tables share their prototype's index: it holds
/// positions only, never keys (each table's slots hold its own keys, which
/// it marks), so a shared index carries no GC reference and outlives the
/// runtime that built it safely. Every table built from one prototype has
/// its keys at the same positions until its first structural change, and
/// every index write goes through `owned`, which detaches first. A value
/// write, a pinned initialization and a lookup never detach.
///
/// The detached copy is a clone, which keeps the bucket count, layout and
/// growth headroom, so `resident_bytes` reports the same size shared or
/// owned: each table is charged the full index whether or not it shares the
/// allocation, and detaching moves no collection score.
#[derive(Debug)]
enum Index {
    Owned(HashTable<usize>),
    Shared(Arc<HashTable<usize>>),
}

impl Default for Index {
    fn default() -> Self {
        Self::Owned(HashTable::new())
    }
}

impl Index {
    #[inline(always)]
    fn get(&self) -> &HashTable<usize> {
        match self {
            Self::Owned(table) => table,
            Self::Shared(table) => table,
        }
    }

    /// The index for a write, detaching a shared one first.
    fn owned(&mut self) -> &mut HashTable<usize> {
        if let Self::Shared(table) = self {
            *self = Self::Owned(HashTable::clone(table));
        }
        match self {
            Self::Owned(table) => table,
            Self::Shared(_) => unreachable!("detached above"),
        }
    }
}

impl HashSlots {
    /// Room for `capacity` slots. Infallible, like all hash-part allocation
    /// (`Table::reserve_hash` says why).
    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            index: Index::Owned(HashTable::with_capacity(capacity)),
        }
    }

    /// Make the index shareable, so `copy_with_capacity` hands copies the
    /// same allocation instead of a clone (`Index` says why that is sound).
    pub(super) fn share_index(&mut self) {
        if let Index::Owned(table) = &mut self.index {
            self.index = Index::Shared(Arc::new(std::mem::take(table)));
        }
    }

    /// A copy of these slots and their index, with room for exactly
    /// `capacity` slots (at least `len`). A shared index is shared again;
    /// an owned one is cloned, which keeps the bucket count and layout.
    /// Either way the copy is indistinguishable from the same pushes made
    /// into `with_capacity(capacity)`: same order, same `resident_bytes`. Constructor templates use it to skip rehashing and
    /// reprobing every key on every construction.
    pub(super) fn copy_with_capacity(&self, capacity: usize) -> Self {
        debug_assert!(capacity >= self.entries.len());
        let mut entries = Vec::with_capacity(capacity);
        entries.extend_from_slice(&self.entries);
        let index = match &self.index {
            Index::Owned(table) => Index::Owned(table.clone()),
            Index::Shared(table) => Index::Shared(Arc::clone(table)),
        };
        Self { entries, index }
    }

    /// The number of slots, vacant ones included.
    #[inline]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Every slot in physical order. A vacant slot reads `(nil, nil)`.
    #[inline]
    pub(super) fn slots(&self) -> &[(Val, Val)] {
        &self.entries
    }

    /// Entry capacity times a fixed literal that covers the slot and its
    /// share of the index. The index is not charged by its own size:
    /// hashbrown's `allocation_size()` follows the group width and pointer
    /// width, so it differs between SSE2, NEON and generic backends and
    /// between hashbrown versions, and collection timing is replay-visible.
    /// Index growth and rebuilds therefore move no score.
    pub(super) fn resident_bytes(&self) -> u64 {
        self.entries.capacity() as u64 * RESIDENT_BYTES_PER_HASH_ENTRY
    }

    pub(super) fn reserve(&mut self, additional: usize, resident: &mut ResidentBytes) {
        let old = self.resident_bytes();
        self.entries.reserve(additional);
        let index = self.index.get();
        // Room already there needs no write, so a shared index stays shared.
        if index.capacity() - index.len() < additional {
            let entries = &self.entries;
            self.index
                .owned()
                .reserve(additional, |&at| hash_key(entries[at].0));
        }
        resident.add(self.resident_bytes() - old);
    }

    /// The position of real key `key`, live or dead. Nil and NaN find
    /// nothing: vacant slots are not indexed, and NaN equals no key.
    #[inline]
    pub(super) fn find(&self, key: Val) -> Option<usize> {
        let entries = &self.entries;
        self.index
            .get()
            .find(hash_key(key), |&at| entries[at].0 == key)
            .copied()
    }

    /// The position and value of real key `key`, live or dead: `find`
    /// without a second `Option` step through `get`.
    #[inline(always)]
    pub(super) fn find_slot(&self, key: Val) -> Option<(usize, Val)> {
        let entries = &self.entries;
        let at = *self
            .index
            .get()
            .find(hash_key(key), |&at| entries[at].0 == key)?;
        Some((at, entries[at].1))
    }

    /// The slot at `position`.
    #[inline]
    pub(super) fn get(&self, position: usize) -> Option<&(Val, Val)> {
        self.entries.get(position)
    }

    /// The key and a mutable value at `position`. Writing a value never
    /// changes which key a slot holds, so the index stays valid.
    #[inline]
    pub(super) fn get_mut(&mut self, position: usize) -> Option<(Val, &mut Val)> {
        self.entries
            .get_mut(position)
            .map(|(key, value)| (*key, value))
    }

    /// Append real key `key`, which the caller has proved absent (by a
    /// missed lookup, or a compaction after which the key has no slot).
    /// Returns its position.
    pub(super) fn push_new(&mut self, key: Val, value: Val) -> usize {
        debug_assert!(super::is_valid_table_key(&key));
        debug_assert!(self.find(key).is_none(), "push_new requires an absent key");
        let position = self.entries.len();
        self.entries.push((key, value));
        let entries = &self.entries;
        self.index
            .owned()
            .insert_unique(hash_key(key), position, |&at| hash_key(entries[at].0));
        position
    }

    /// Append to published backing, charging any entry-vector growth.
    pub(super) fn push_tracked(
        &mut self,
        key: Val,
        value: Val,
        resident: &mut ResidentBytes,
    ) -> usize {
        if self.entries.len() < self.entries.capacity() {
            return self.push_new(key, value);
        }
        let old = self.resident_bytes();
        let position = self.push_new(key, value);
        resident.add(self.resident_bytes() - old);
        position
    }

    /// Append real key `key` unless it already has a slot. Returns the
    /// position and whether a slot was added.
    pub(super) fn push_if_absent(&mut self, key: Val, value: Val) -> (usize, bool) {
        match self.find(key) {
            Some(position) => (position, false),
            None => (self.push_new(key, value), true),
        }
    }

    /// Append a vacant slot (a snapshot restoring one).
    #[cfg(feature = "snapshot")]
    pub(super) fn push_vacant(&mut self) {
        self.entries.push((Val::NIL, Val::NIL));
    }

    /// Find real key `key`, live or dead, and hold its index entry for a
    /// write. The handle borrows these slots, so nothing can touch the
    /// index between this probe and a `relocate`; that is what lets the
    /// re-add retarget the entry without probing for it again. A shared
    /// index is probed read-only and stays shared: most found keys only
    /// take a value, and only a `relocate` detaches and probes again.
    #[inline]
    pub(super) fn find_mut(&mut self, key: Val) -> Option<SlotMut<'_>> {
        let Self { entries, index } = self;
        if let Index::Shared(table) = index {
            let position = *table.find(hash_key(key), |&at| entries[at].0 == key)?;
            return Some(SlotMut {
                entries,
                indexed: Indexed::Shared { index, position },
            });
        }
        let Index::Owned(table) = index else {
            unreachable!("shared handled above");
        };
        let indexed = table.find_mut(hash_key(key), |&at| entries[at].0 == key)?;
        Some(SlotMut {
            entries,
            indexed: Indexed::Owned(indexed),
        })
    }

    /// Drop every nil-valued slot (dead and vacant), keeping the others in
    /// order, and rebuild the index for the new positions.
    pub(super) fn retain_live(&mut self) {
        self.entries.retain(|(_, value)| !value.is_nil());
        self.rebuild_index();
    }

    /// Index every real key at its current position.
    fn rebuild_index(&mut self) {
        let index = self.index.owned();
        index.clear();
        let entries = &self.entries;
        for (position, (key, _)) in entries.iter().enumerate() {
            if !key.is_nil() {
                index.insert_unique(hash_key(*key), position, |&at| hash_key(entries[at].0));
            }
        }
    }

    /// Debug check: every real key probes back to its own slot, and the
    /// index holds nothing else.
    #[cfg(debug_assertions)]
    pub(super) fn index_is_exact(&self) -> bool {
        let mut real = 0;
        for (position, (key, value)) in self.entries.iter().enumerate() {
            if key.is_nil() {
                if !value.is_nil() {
                    return false;
                }
                continue;
            }
            real += 1;
            if self.find(*key) != Some(position) {
                return false;
            }
        }
        real == self.index.get().len()
    }

    /// Test helper: swap two slots (the index follows), the kind of reorder
    /// the shape witness must catch when the version does not move.
    #[cfg(all(test, debug_assertions))]
    pub(super) fn swap_slots_for_test(&mut self, a: usize, b: usize) {
        self.entries.swap(a, b);
        self.rebuild_index();
    }

    /// Test helper: vacate a slot (the index follows) without the version
    /// move a real vacate makes.
    #[cfg(all(test, debug_assertions))]
    pub(super) fn vacate_for_test(&mut self, position: usize) {
        self.entries[position] = (Val::NIL, Val::NIL);
        self.rebuild_index();
    }
}

/// A found key's slot together with its index entry, from
/// `HashSlots::find_mut`.
pub(super) struct SlotMut<'a> {
    entries: &'a mut Vec<(Val, Val)>,
    indexed: Indexed<'a>,
}

/// The index entry a `SlotMut` holds: the bucket itself in an owned index,
/// or the position and the index to detach in a shared one.
enum Indexed<'a> {
    Owned(&'a mut usize),
    Shared {
        index: &'a mut Index,
        position: usize,
    },
}

impl SlotMut<'_> {
    /// The key's position.
    #[inline]
    pub(super) fn position(&self) -> usize {
        match &self.indexed {
            Indexed::Owned(indexed) => **indexed,
            Indexed::Shared { position, .. } => *position,
        }
    }

    /// The key's value. Writing it never changes which key the slot holds,
    /// so the index stays valid.
    #[inline]
    pub(super) fn value_mut(&mut self) -> &mut Val {
        let position = self.position();
        &mut self.entries[position].1
    }

    /// Move the key to a new slot at the back holding `value`, leaving its
    /// old slot vacant. The index entry the lookup found is retargeted in
    /// place: nothing is probed, erased or inserted. Returns the new
    /// position.
    pub(super) fn relocate(self, value: Val, resident: &mut ResidentBytes) -> usize {
        let old = self.position();
        let key = self.entries[old].0;
        debug_assert!(super::is_valid_table_key(&key));
        let position = self.entries.len();
        let capacity = self.entries.capacity();
        // Push first: a failed allocation leaves the old binding intact.
        self.entries.push((key, value));
        if self.entries.capacity() != capacity {
            resident
                .add((self.entries.capacity() - capacity) as u64 * RESIDENT_BYTES_PER_HASH_ENTRY);
        }
        match self.indexed {
            Indexed::Owned(indexed) => *indexed = position,
            Indexed::Shared { index, .. } => {
                // Detaching keeps the charged size (`Index`). The old
                // position is indexed under the key's hash.
                let indexed = index
                    .owned()
                    .find_mut(hash_key(key), |&at| at == old)
                    .expect("a shared index holds the relocated key's old position");
                *indexed = position;
            }
        }
        self.entries[old] = (Val::NIL, Val::NIL);
        position
    }
}
