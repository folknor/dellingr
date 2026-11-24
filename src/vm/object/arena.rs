//! Generational arena for heap objects and interned strings, and the
//! occupancy bitmaps the upvalue pool shares with it.
//!
//! The arena replaces `slotmap::SlotMap` for the two swept pools so a sweep
//! visits only slots that hold something. `SlotMap::retain` walks every slot
//! up to the map's high-water mark, vacant or not, so a State that once held
//! many objects paid for all of those slots on every later collection, and
//! with the default pacing collections come every few dozen allocations once
//! the live set is small. Here a two-level occupancy bitmap (one bit per slot,
//! one summary bit per non-empty word) drives every traversal, and the mark
//! bits live beside it, so a sweep reads one summary bit per 4096 slots, one
//! word per occupied 64-slot run, and touches only the payloads it frees.
//!
//! Handles, reuse order and version stepping reproduce slotmap 1.1's
//! `SlotMap` exactly, which is what keeps this change unobservable:
//!
//! - slot 0 is an unused sentinel; new slots append at version 1;
//! - a removal steps the version by one (wrapping, so even means vacant) and
//!   pushes the index on a LIFO free stack, the order slotmap's `free_head`
//!   list gives; an insert pops it and steps the version by one more (to odd),
//!   so one collection cycle moves a reused slot's version by exactly two
//!   (`CACHE_PRUNE_INTERVAL` rests on that);
//! - a sweep frees dead slots in ascending index order, as `retain` did;
//! - keys hash as one `write_u64(version << 32 | idx)` and order by
//!   `(idx, version)`, as `slotmap::KeyData` does, so table hash layout is
//!   unchanged too.
//!
//! The `arena_matches_slotmap` test drives both side by side.

use std::cell::Cell;
use std::hash::{Hash, Hasher};
use std::num::NonZeroU32;

/// A generational handle: slot index plus the slot's (odd) version when the
/// value was inserted. A freed slot's version moves on, so an old handle
/// stops resolving (until the version wraps after 2^31 reuses).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SlotKey {
    idx: u32,
    version: NonZeroU32,
}

impl SlotKey {
    /// The key no slot ever carries.
    pub(crate) const NULL: Self = Self {
        idx: u32::MAX,
        version: NonZeroU32::MIN,
    };

    /// Slot index and version, for tests that pin the stepping.
    #[cfg(test)]
    pub(crate) fn parts(self) -> (u32, u32) {
        (self.idx, self.version.get())
    }
}

impl Hash for SlotKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64((u64::from(self.version.get()) << 32) | u64::from(self.idx));
    }
}

/// A typed key over [`SlotKey`], so object and string handles cannot mix.
pub(crate) trait ArenaKey: Copy {
    fn from_slot(key: SlotKey) -> Self;
    fn slot(self) -> SlotKey;
}

/// One bit per slot: which slots are occupied, a summary bit per non-empty
/// occupancy word, and the mark bits of the current collection (always a
/// subset of the occupied bits; `Cell` because marking runs through a shared
/// borrow).
#[derive(Default)]
pub(crate) struct Occupancy {
    occupied: Vec<u64>,
    summary: Vec<u64>,
    reachable: Vec<Cell<u64>>,
}

impl Occupancy {
    /// Record `idx` as occupied and unmarked, growing the bitmaps if needed.
    #[inline]
    pub(crate) fn occupy(&mut self, idx: usize) {
        let word = idx / 64;
        if word >= self.occupied.len() {
            self.grow(word);
        }
        debug_assert_eq!(self.reachable[word].get() & (1 << (idx % 64)), 0);
        self.occupied[word] |= 1 << (idx % 64);
        self.summary[word / 64] |= 1 << (word % 64);
    }

    #[cold]
    fn grow(&mut self, word: usize) {
        self.occupied.resize(word + 1, 0);
        self.reachable.resize_with(word + 1, || Cell::new(0));
        self.summary.resize(word / 64 + 1, 0);
    }

    #[inline]
    pub(crate) fn is_occupied(&self, idx: usize) -> bool {
        self.occupied
            .get(idx / 64)
            .is_some_and(|word| word & (1 << (idx % 64)) != 0)
    }

    /// Set the mark bit of an occupied slot; true when it was unmarked.
    #[inline]
    pub(crate) fn mark(&self, idx: usize) -> bool {
        let cell = &self.reachable[idx / 64];
        let bit = 1 << (idx % 64);
        let word = cell.get();
        cell.set(word | bit);
        word & bit == 0
    }

    #[inline]
    pub(crate) fn is_marked(&self, idx: usize) -> bool {
        self.reachable[idx / 64].get() & (1 << (idx % 64)) != 0
    }

    /// Occupied slot indices in ascending order, skipping empty words.
    #[cfg(any(debug_assertions, test))]
    pub(crate) fn indices(&self) -> Indices<'_> {
        Indices {
            occupied: &self.occupied,
            summary: &self.summary,
            summary_idx: 0,
            summary_bits: self.summary.first().copied().unwrap_or(0),
            word_idx: 0,
            word_bits: 0,
        }
    }

    /// Clear every mark bit without freeing anything.
    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn clear_marks(&self) {
        for cell in &self.reachable {
            cell.set(0);
        }
    }

    /// End a collection: every occupied, unmarked slot becomes vacant and is
    /// handed to `free` in ascending index order; marks are cleared. Only
    /// non-empty words are visited. A word's bitmaps are committed before
    /// its slots are freed, so `free` sees consistent occupancy even if it
    /// unwinds.
    pub(crate) fn sweep(&mut self, mut free: impl FnMut(usize)) {
        for summary_idx in 0..self.summary.len() {
            let mut summary_bits = self.summary[summary_idx];
            while summary_bits != 0 {
                let bit = summary_bits.trailing_zeros() as usize;
                summary_bits &= summary_bits - 1;
                let word = summary_idx * 64 + bit;
                let occupied = self.occupied[word];
                let reachable = self.reachable[word].replace(0);
                let live = occupied & reachable;
                let mut dead = occupied & !reachable;
                self.occupied[word] = live;
                if live == 0 {
                    self.summary[summary_idx] &= !(1 << bit);
                }
                while dead != 0 {
                    // bounded: at most 64 bits per word.
                    free(word * 64 + dead.trailing_zeros() as usize);
                    dead &= dead - 1;
                }
            }
        }
    }
}

/// Ascending iterator over the occupied indices of an [`Occupancy`]. Only
/// debug checks and tests enumerate a pool; the collector sweeps instead.
#[cfg(any(debug_assertions, test))]
pub(crate) struct Indices<'a> {
    occupied: &'a [u64],
    summary: &'a [u64],
    summary_idx: usize,
    summary_bits: u64,
    word_idx: usize,
    word_bits: u64,
}

#[cfg(any(debug_assertions, test))]
impl Iterator for Indices<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        loop {
            if self.word_bits != 0 {
                let bit = self.word_bits.trailing_zeros() as usize;
                self.word_bits &= self.word_bits - 1;
                return Some(self.word_idx * 64 + bit);
            }
            if self.summary_bits != 0 {
                let bit = self.summary_bits.trailing_zeros() as usize;
                self.summary_bits &= self.summary_bits - 1;
                self.word_idx = self.summary_idx * 64 + bit;
                self.word_bits = self.occupied[self.word_idx];
                continue;
            }
            self.summary_idx += 1;
            self.summary_bits = *self.summary.get(self.summary_idx)?;
        }
    }
}

/// Visible to the parent module so it can pin a pool's slot stride.
pub(super) struct Slot<V> {
    /// Odd while occupied, even while vacant (slotmap's convention).
    version: u32,
    value: Option<V>,
}

/// Generational arena; see the module docs for the slotmap equivalence.
pub(crate) struct Arena<K, V> {
    slots: Vec<Slot<V>>,
    free: Vec<u32>,
    bits: Occupancy,
    len: usize,
    _key: std::marker::PhantomData<fn() -> K>,
}

impl<K: ArenaKey, V> Arena<K, V> {
    pub(crate) fn new() -> Self {
        Self {
            slots: vec![Slot {
                version: 0,
                value: None,
            }],
            free: Vec::new(),
            bits: Occupancy::default(),
            len: 0,
            _key: std::marker::PhantomData,
        }
    }

    /// Live values.
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Room for `additional` more values without reallocating, counting
    /// vacant slots as room (as `SlotMap::reserve` does).
    pub(crate) fn reserve(&mut self, additional: usize) {
        let needed = (self.len + additional).saturating_sub(self.slots.len() - 1);
        self.slots.reserve(needed);
    }

    #[inline]
    pub(crate) fn insert(&mut self, value: V) -> K {
        self.len += 1;
        if let Some(idx) = self.free.pop() {
            let slot = &mut self.slots[idx as usize];
            slot.version |= 1;
            slot.value = Some(value);
            self.bits.occupy(idx as usize);
            return K::from_slot(SlotKey {
                idx,
                version: NonZeroU32::new(slot.version).expect("an odd version is nonzero"),
            });
        }
        // u32::MAX is the null key's index, so the last usable slot is below it.
        let idx = match u32::try_from(self.slots.len()) {
            Ok(idx) if idx < u32::MAX => idx,
            _ => panic!("arena slot count overflows u32"),
        };
        self.slots.push(Slot {
            version: 1,
            value: Some(value),
        });
        self.bits.occupy(idx as usize);
        K::from_slot(SlotKey {
            idx,
            version: NonZeroU32::MIN,
        })
    }

    #[inline]
    fn slot_index(&self, key: K) -> Option<usize> {
        let key = key.slot();
        let idx = key.idx as usize;
        let slot = self.slots.get(idx)?;
        (slot.version == key.version.get()).then_some(idx)
    }

    #[inline]
    pub(crate) fn get(&self, key: K) -> Option<&V> {
        let idx = self.slot_index(key)?;
        self.slots[idx].value.as_ref()
    }

    #[inline]
    pub(crate) fn get_mut(&mut self, key: K) -> Option<&mut V> {
        let idx = self.slot_index(key)?;
        self.slots[idx].value.as_mut()
    }

    #[inline]
    pub(crate) fn contains(&self, key: K) -> bool {
        self.slot_index(key).is_some()
    }

    /// Mark a live value for the current collection: `Some(true)` on its
    /// first mark, `Some(false)` after, `None` for a stale or null key (which
    /// never marks the slot's current occupant).
    #[inline]
    pub(crate) fn mark(&self, key: K) -> Option<bool> {
        self.slot_index(key).map(|idx| self.bits.mark(idx))
    }

    /// Whether a live value is marked; `None` for a stale or null key.
    pub(crate) fn is_marked(&self, key: K) -> Option<bool> {
        self.slot_index(key).map(|idx| self.bits.is_marked(idx))
    }

    /// Live values with their keys, in ascending slot order.
    #[cfg(any(debug_assertions, test))]
    pub(crate) fn iter(&self) -> impl Iterator<Item = (K, &V)> {
        self.bits.indices().map(|idx| {
            let slot = &self.slots[idx];
            let key = K::from_slot(SlotKey {
                idx: idx as u32,
                version: NonZeroU32::new(slot.version).expect("an occupied slot's version is odd"),
            });
            (
                key,
                slot.value.as_ref().expect("an occupied bit names a value"),
            )
        })
    }

    #[cfg(any(debug_assertions, test))]
    pub(crate) fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, value)| value)
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn clear_marks(&self) {
        self.bits.clear_marks();
    }

    /// Free every unmarked value in ascending slot order and clear the marks.
    /// `dead` sees each freed value with its key after the slot is already
    /// vacant (so the key no longer resolves) and before the value drops.
    pub(crate) fn sweep(&mut self, mut dead: impl FnMut(K, &V)) {
        let slots = &mut self.slots;
        let free = &mut self.free;
        let len = &mut self.len;
        self.bits.sweep(|idx| {
            let slot = &mut slots[idx];
            let key = K::from_slot(SlotKey {
                idx: idx as u32,
                version: NonZeroU32::new(slot.version).expect("an occupied slot's version is odd"),
            });
            let value = slot.value.take().expect("an occupied bit names a value");
            slot.version = slot.version.wrapping_add(1);
            *len -= 1;
            free.push(idx as u32);
            dead(key, &value);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct TestKey(SlotKey);

    impl ArenaKey for TestKey {
        fn from_slot(key: SlotKey) -> Self {
            Self(key)
        }
        fn slot(self) -> SlotKey {
            self.0
        }
    }

    fn ffi(key: SlotKey) -> u64 {
        (u64::from(key.version.get()) << 32) | u64::from(key.idx)
    }

    /// Drive the arena and a `slotmap::SlotMap` through the same inserts and
    /// sweeps, crossing word (64) and summary (4096) boundaries, emptying a
    /// whole word and reusing into it while older vacancies wait: every key,
    /// every freeing order and every iteration must agree.
    #[test]
    fn arena_matches_slotmap() {
        use slotmap::{Key as _, SlotMap};
        let mut arena: Arena<TestKey, u32> = Arena::new();
        let mut reference: SlotMap<slotmap::DefaultKey, u32> = SlotMap::new();
        let mut keys: Vec<(TestKey, slotmap::DefaultKey)> = Vec::new();
        let mut next = 0u32;
        let mut insert = |arena: &mut Arena<TestKey, u32>,
                          reference: &mut SlotMap<slotmap::DefaultKey, u32>,
                          keys: &mut Vec<(TestKey, slotmap::DefaultKey)>,
                          count: usize| {
            for _ in 0..count {
                let a = arena.insert(next);
                let r = reference.insert(next);
                assert_eq!(ffi(a.0), r.data().as_ffi());
                keys.push((a, r));
                next += 1;
            }
        };
        insert(&mut arena, &mut reference, &mut keys, 9000);
        // Rounds of keep-predicates over the value: thin out, empty the
        // second word entirely, keep only a high survivor, then everything.
        let rounds: [&dyn Fn(u32) -> bool; 5] = [
            &|v| v % 3 != 0,
            &|v| !(64..128).contains(&v) && v % 5 != 0,
            &|v| !(4095..=4097).contains(&v),
            &|v| v == 8999 || v >= 9000,
            &|_| false,
        ];
        for keep in rounds {
            for (a, _) in &keys {
                if let Some(value) = arena.get(*a)
                    && keep(*value)
                {
                    assert_eq!(arena.mark(*a), Some(true));
                    assert_eq!(arena.mark(*a), Some(false));
                }
            }
            let mut freed = Vec::new();
            arena.sweep(|key, value| freed.push((ffi(key.0), *value)));
            let mut reference_freed = Vec::new();
            reference.retain(|key, value| {
                let kept = keep(*value);
                if !kept {
                    reference_freed.push((key.data().as_ffi(), *value));
                }
                kept
            });
            assert_eq!(freed, reference_freed);
            assert_eq!(arena.len(), reference.len());
            let live: Vec<_> = arena.iter().map(|(k, v)| (ffi(k.0), *v)).collect();
            let reference_live: Vec<_> = reference
                .iter()
                .map(|(k, v)| (k.data().as_ffi(), *v))
                .collect();
            assert_eq!(live, reference_live);
            for (a, r) in &keys {
                assert_eq!(arena.contains(*a), reference.contains_key(*r));
                if !arena.contains(*a) {
                    assert_eq!(arena.mark(*a), None, "a stale key marks nothing");
                }
            }
            insert(&mut arena, &mut reference, &mut keys, 100);
        }
    }

    #[test]
    fn version_wraps_like_slotmap() {
        let mut arena: Arena<TestKey, ()> = Arena::new();
        let key = arena.insert(());
        arena.slots[1].version = u32::MAX;
        let stale = TestKey(SlotKey {
            idx: 1,
            version: NonZeroU32::new(u32::MAX).expect("nonzero"),
        });
        assert!(arena.contains(stale));
        assert!(!arena.contains(key));
        arena.sweep(|_, ()| {});
        assert_eq!(arena.slots[1].version, 0);
        let reused = arena.insert(());
        assert_eq!(reused.0.parts(), (1, 1));
        assert!(!arena.contains(stale));
    }

    #[test]
    fn sweep_skips_empty_words() {
        let mut bits = Occupancy::default();
        bits.occupy(5);
        bits.occupy(70_000);
        bits.mark(70_000);
        let mut freed = Vec::new();
        bits.sweep(|idx| freed.push(idx));
        assert_eq!(freed, vec![5]);
        assert_eq!(bits.indices().collect::<Vec<_>>(), vec![70_000]);
        bits.sweep(|idx| freed.push(idx));
        assert_eq!(freed, vec![5, 70_000]);
        assert_eq!(bits.indices().count(), 0);
        bits.occupy(64);
        assert_eq!(bits.indices().collect::<Vec<_>>(), vec![64]);
    }
}
