use std::cell::Cell;

use indexmap::IndexMap;

use super::Error;
use super::Result;
use super::TypeError;
use super::Val;
use super::object::{GcHeap, Markable, ObjectPtr, UpvaluePool};

/// Maximum number of entries for inline storage.
/// Tables with more entries promote to IndexMap.
const INLINE_CAPACITY: usize = 4;

/// Storage for table entries. Small tables (≤4 entries) use inline array storage
/// for better cache locality and reduced allocation overhead. Larger tables
/// use IndexMap to maintain insertion order for deterministic `pairs()` iteration.
#[derive(Debug)]
enum TableStorage {
    /// Inline storage for small tables. Stores key-value pairs directly.
    /// `len` tracks how many slots are used (0..=INLINE_CAPACITY).
    Inline {
        entries: [(Val, Val); INLINE_CAPACITY],
        len: u8,
    },
    /// IndexMap storage for larger tables. Maintains insertion order.
    Map(IndexMap<Val, Val>),
}

/// The result of advancing a table iterator.
pub(super) enum TableNext {
    Pair(Val, Val),
    End,
    InvalidKey,
}

impl Default for TableStorage {
    fn default() -> Self {
        TableStorage::Inline {
            entries: Default::default(),
            len: 0,
        }
    }
}

/// A Lua table with optimized storage for small tables.
/// Tables with ≤4 entries use inline array storage for better performance.
/// Larger tables use IndexMap to maintain insertion order for `pairs()`.
#[derive(Debug)]
pub(super) struct Table {
    storage: TableStorage,
    metatable: Option<ObjectPtr>,
    /// Shape version for key/index stability. Value updates do not bump this.
    version: Cell<u64>,
    /// Cached array length. None means cache is invalid and needs recomputation.
    /// Invalidated when positive integer keys are inserted or removed.
    /// Uses Cell for interior mutability so array_len() can cache on &self.
    cached_array_len: Cell<Option<usize>>,
    /// Number of occupied storage slots whose nil value makes them logically absent.
    dead_count: usize,
}

impl Default for Table {
    fn default() -> Self {
        Self {
            storage: TableStorage::default(),
            metatable: None,
            version: Cell::new(0),
            cached_array_len: Cell::new(None),
            dead_count: 0,
        }
    }
}

impl Table {
    pub(super) fn with_capacity(capacity: usize) -> Self {
        if capacity <= INLINE_CAPACITY {
            Self::default()
        } else {
            Self {
                storage: TableStorage::Map(IndexMap::with_capacity(capacity)),
                metatable: None,
                version: Cell::new(0),
                cached_array_len: Cell::new(None),
                dead_count: 0,
            }
        }
    }

    pub(super) fn with_template_keys(
        key_ids: &[u8],
        string_literals: &[Val],
        string_literal_start: usize,
    ) -> Self {
        let mut map = IndexMap::with_capacity(key_ids.len());
        for key_id in key_ids {
            let key = string_literals[string_literal_start + *key_id as usize];
            map.insert(key, Val::Nil);
        }
        Self {
            storage: TableStorage::Map(map),
            metatable: None,
            version: Cell::new(0),
            cached_array_len: Cell::new(None),
            dead_count: key_ids.len(),
        }
    }

    /// Check if a value is a positive integer (potential array index).
    #[inline]
    #[allow(clippy::float_cmp)]
    fn is_array_key(key: &Val) -> bool {
        if let Val::Num(n) = key {
            *n > 0.0 && n.is_finite() && *n == n.floor()
        } else {
            false
        }
    }

    #[inline]
    pub(super) fn version(&self) -> u64 {
        self.version.get()
    }

    #[inline]
    fn bump_version(&self) {
        self.version.set(self.version.get().wrapping_add(1));
    }

    #[hotpath::measure]
    pub(super) fn get(&self, key: &Val) -> Val {
        match key {
            Val::Nil => Val::Nil,
            Val::Num(n) if n.is_nan() => Val::Nil,
            _ => match &self.storage {
                TableStorage::Inline { entries, len } => {
                    for (entry_key, entry_value) in entries.iter().take(*len as usize) {
                        if entry_key == key {
                            return *entry_value;
                        }
                    }
                    Val::Nil
                }
                TableStorage::Map(map) => {
                    if Self::is_array_key(key)
                        && let Val::Num(n) = key
                    {
                        let idx = (*n as usize) - 1;
                        if let Some((Val::Num(kn), v)) = map.get_index(idx)
                            && kn.to_bits() == n.to_bits()
                        {
                            return *v;
                        }
                    }
                    map.get(key).copied().unwrap_or_default()
                }
            },
        }
    }

    #[inline]
    pub(super) fn get_with_index(&self, key: &Val) -> Option<(usize, Val)> {
        match key {
            Val::Nil => None,
            Val::Num(n) if n.is_nan() => None,
            _ => match &self.storage {
                TableStorage::Inline { entries, len } => {
                    for (idx, (entry_key, entry_value)) in
                        entries.iter().take(*len as usize).enumerate()
                    {
                        if entry_key == key {
                            return (!matches!(entry_value, Val::Nil))
                                .then_some((idx, *entry_value));
                        }
                    }
                    None
                }
                TableStorage::Map(map) => {
                    let idx = map.get_index_of(key)?;
                    let (_, value) = map.get_index(idx)?;
                    (!matches!(value, Val::Nil)).then_some((idx, *value))
                }
            },
        }
    }

    #[inline]
    pub(super) fn get_index(&self, index: usize) -> Option<(Val, Val)> {
        match &self.storage {
            TableStorage::Inline { entries, len } => {
                if index < *len as usize {
                    (!matches!(entries[index].1, Val::Nil)).then_some(entries[index])
                } else {
                    None
                }
            }
            TableStorage::Map(map) => map
                .get_index(index)
                .and_then(|(key, value)| (!matches!(value, Val::Nil)).then_some((*key, *value))),
        }
    }

    #[cfg(feature = "snapshot")]
    pub(super) fn entries(&self) -> Vec<(Val, Val)> {
        match &self.storage {
            TableStorage::Inline { entries, len } => entries
                .iter()
                .take(*len as usize)
                .filter(|(_, value)| !matches!(value, Val::Nil))
                .copied()
                .collect(),
            TableStorage::Map(map) => map
                .iter()
                .filter(|(_, value)| !matches!(value, Val::Nil))
                .map(|(key, value)| (*key, *value))
                .collect(),
        }
    }

    #[cfg(feature = "snapshot")]
    pub(super) fn clear_and_insert_entries(&mut self, entries: Vec<(Val, Val)>) -> Result<()> {
        // Reset to a shell sized for the incoming entries, then re-insert in
        // order so `pairs()` iteration order round-trips. with_capacity picks
        // inline vs map storage from the count.
        *self = Table::with_capacity(entries.len());
        for (key, value) in entries {
            self.insert(key, value)?;
        }
        Ok(())
    }

    /// Update the value at a specific entry index. Returns true on success.
    ///
    /// This is a hot-path helper for the OP_SET_FIELD inline cache: when a
    /// callsite has already verified that a key lives at a known index, the
    /// IC writes the new value through this method without re-doing the key
    /// lookup. Caller must ensure the value is non-nil (assigning nil is
    /// remove-semantics and must go through the slow path) and that the
    /// entry at `index` corresponds to the intended key (typically verified
    /// by table_version match or get_index key compare). Does not bump
    /// version (value-only change preserves (key, index) bindings) and does
    /// not invalidate cached_array_len (presence of any key unchanged).
    #[inline]
    pub(super) fn set_at_index(&mut self, index: usize, value: Val) -> bool {
        match &mut self.storage {
            TableStorage::Inline { entries, len } => {
                if index < *len as usize && !matches!(entries[index].1, Val::Nil) {
                    entries[index].1 = value;
                    true
                } else {
                    false
                }
            }
            TableStorage::Map(map) => {
                let Some((_, v)) = map.get_index_mut(index) else {
                    return false;
                };
                if matches!(*v, Val::Nil) {
                    false
                } else {
                    *v = value;
                    true
                }
            }
        }
    }

    /// Initializes a reserved template slot without exposing dead slots to caches.
    pub(super) fn init_at_index(&mut self, index: usize, key: Val, value: Val) -> bool {
        let activated = match &mut self.storage {
            TableStorage::Inline { entries, len } if index < *len as usize => {
                let (existing_key, existing_value) = &mut entries[index];
                if *existing_key != key {
                    return false;
                }
                let activated = matches!(*existing_value, Val::Nil) && !matches!(value, Val::Nil);
                *existing_value = value;
                activated
            }
            TableStorage::Map(map) => {
                let Some((existing_key, existing_value)) = map.get_index_mut(index) else {
                    return false;
                };
                if *existing_key != key {
                    return false;
                }
                let activated = matches!(*existing_value, Val::Nil) && !matches!(value, Val::Nil);
                *existing_value = value;
                activated
            }
            TableStorage::Inline { .. } => return false,
        };
        if activated {
            self.dead_count -= 1;
        }
        true
    }

    /// Returns a "border" of the table per the Lua `#` operator.
    /// A border is any non-negative integer N where `t[N]` is non-nil (or N == 0)
    /// and `t[N+1]` is nil. For a sequence (no nil holes) this is the length.
    /// For non-sequences any border is valid; the result is deterministic for
    /// a given table state but may change after inserts/removes.
    /// Uses cached value when available for O(1) performance.
    #[hotpath::measure]
    pub(super) fn array_len(&self) -> usize {
        if let Some(len) = self.cached_array_len.get() {
            return len;
        }
        let len = self.compute_array_len();
        self.cached_array_len.set(Some(len));
        len
    }

    /// Computes a border via exponential doubling + binary search, matching
    /// reference Lua's `luaH_getn`. O(log N) lookups for a dense table of
    /// length N, vs O(N) for a linear scan.
    #[hotpath::measure]
    fn compute_array_len(&self) -> usize {
        // t[1] nil: 0 is a border.
        if matches!(self.get(&Val::Num(1.0)), Val::Nil) {
            return 0;
        }
        // Doubling: find hi such that t[hi] is nil. Invariant: t[lo] non-nil.
        let mut lo: usize = 1;
        let mut hi: usize = 2;
        while !matches!(self.get(&Val::Num(hi as f64)), Val::Nil) {
            lo = hi;
            if hi > usize::MAX / 2 {
                // Pathologically large dense table; fall back to linear scan.
                while !matches!(self.get(&Val::Num((lo + 1) as f64)), Val::Nil) {
                    lo += 1;
                }
                return lo;
            }
            hi *= 2;
        }
        // Binary search [lo, hi): t[lo] non-nil, t[hi] nil.
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if matches!(self.get(&Val::Num(mid as f64)), Val::Nil) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        lo
    }

    #[hotpath::measure]
    pub(super) fn insert(&mut self, key: Val, value: Val) -> Result<()> {
        match &key {
            Val::Nil => return Err(Error::new(TypeError::TableKeyNil, 0, 0)),
            Val::Num(n) if n.is_nan() => return Err(Error::new(TypeError::TableKeyNan, 0, 0)),
            _ => {}
        }

        // In Lua, assigning nil deletes the key rather than storing a nil value.
        if matches!(value, Val::Nil) {
            self.remove(&key);
            return Ok(());
        }

        // Invalidate cache if this could affect array length
        if Self::is_array_key(&key) {
            self.cached_array_len.set(None);
        }

        if self.dead_count == 0 {
            return self.insert_without_tombstones(key, value);
        }

        match &mut self.storage {
            TableStorage::Inline { entries, len } => {
                if let Some(index) = entries
                    .iter()
                    .take(*len as usize)
                    .position(|entry| entry.0 == key && !matches!(entry.1, Val::Nil))
                {
                    entries[index].1 = value;
                    return Ok(());
                }
                if let Some(index) = entries
                    .iter()
                    .take(*len as usize)
                    .position(|entry| entry.0 == key)
                {
                    for shift in index..*len as usize - 1 {
                        entries[shift] = std::mem::take(&mut entries[shift + 1]);
                    }
                    *len -= 1;
                    self.dead_count -= 1;
                    self.bump_version();
                } else if self.dead_count >= *len as usize - self.dead_count {
                    if self.compact_dead() {
                        self.bump_version();
                    }
                } else if *len as usize == INLINE_CAPACITY && self.compact_dead() {
                    self.bump_version();
                }
                let TableStorage::Inline { entries, len } = &mut self.storage else {
                    unreachable!("inline compaction cannot promote storage");
                };
                if (*len as usize) < INLINE_CAPACITY {
                    entries[*len as usize] = (key, value);
                    *len += 1;
                } else {
                    self.promote_to_map(key, value);
                }
            }
            TableStorage::Map(_) => self.insert_into_map_with_tombstones(key, value),
        }
        Ok(())
    }

    fn insert_into_map_with_tombstones(&mut self, key: Val, value: Val) {
        let index = match &self.storage {
            TableStorage::Map(map) => map.get_index_of(&key),
            TableStorage::Inline { .. } => unreachable!("map insertion requires map storage"),
        };
        if let Some(index) = index {
            let was_live = match &mut self.storage {
                TableStorage::Map(map) => {
                    let (_, existing) = map
                        .get_index_mut(index)
                        .expect("IndexMap index returned by get_index_of must be valid");
                    if !matches!(*existing, Val::Nil) {
                        *existing = value;
                        true
                    } else {
                        map.shift_remove_index(index);
                        false
                    }
                }
                TableStorage::Inline { .. } => unreachable!("map insertion requires map storage"),
            };
            if was_live {
                return;
            }
            self.dead_count -= 1;
            self.bump_version();
        } else {
            let live_count = match &self.storage {
                TableStorage::Map(map) => map.len() - self.dead_count,
                TableStorage::Inline { .. } => unreachable!("map insertion requires map storage"),
            };
            if self.dead_count >= live_count && self.compact_dead() {
                self.bump_version();
            }
        }
        if let TableStorage::Map(map) = &mut self.storage {
            map.insert(key, value);
        }
    }

    fn insert_without_tombstones(&mut self, key: Val, value: Val) -> Result<()> {
        match &mut self.storage {
            TableStorage::Inline { entries, len } => {
                for (entry_key, entry_value) in entries.iter_mut().take(*len as usize) {
                    if *entry_key == key {
                        *entry_value = value;
                        return Ok(());
                    }
                }
                if (*len as usize) < INLINE_CAPACITY {
                    entries[*len as usize] = (key, value);
                    *len += 1;
                } else {
                    self.promote_to_map(key, value);
                }
            }
            TableStorage::Map(map) => {
                map.insert(key, value);
            }
        }
        Ok(())
    }

    /// Promote from inline storage to IndexMap, adding the new key-value pair.
    #[hotpath::measure]
    fn promote_to_map(&mut self, new_key: Val, new_value: Val) {
        debug_assert_eq!(self.dead_count, 0);
        let old_storage = std::mem::take(&mut self.storage);
        if let TableStorage::Inline { mut entries, len } = old_storage {
            let mut map = IndexMap::with_capacity(INLINE_CAPACITY + 1);
            for entry in entries.iter_mut().take(len as usize) {
                let (k, v) = std::mem::take(entry);
                map.insert(k, v);
            }
            map.insert(new_key, new_value);
            self.storage = TableStorage::Map(map);
        }
    }

    /// Ensure storage is Map (for operations that need IndexMap's shift_remove).
    #[hotpath::measure]
    fn ensure_map(&mut self) {
        debug_assert_eq!(self.dead_count, 0);
        // Only convert if currently Inline
        if matches!(self.storage, TableStorage::Inline { .. })
            && let TableStorage::Inline { mut entries, len } = std::mem::take(&mut self.storage)
        {
            let mut map = IndexMap::with_capacity(len as usize);
            for entry in entries.iter_mut().take(len as usize) {
                let (k, v) = std::mem::take(entry);
                map.insert(k, v);
            }
            self.storage = TableStorage::Map(map);
        }
    }

    /// Remove a key and return its value (if any).
    #[hotpath::measure]
    fn remove(&mut self, key: &Val) -> Option<Val> {
        if Self::is_array_key(key) {
            self.cached_array_len.set(None);
        }

        match &mut self.storage {
            TableStorage::Inline { entries, len } => {
                for (entry_key, entry_value) in entries.iter_mut().take(*len as usize) {
                    if entry_key == key {
                        let removed = std::mem::replace(entry_value, Val::Nil);
                        if !matches!(removed, Val::Nil) {
                            self.dead_count += 1;
                            return Some(removed);
                        }
                        return None;
                    }
                }
                None
            }
            TableStorage::Map(map) => {
                let value = map.get_mut(key)?;
                let removed = std::mem::replace(value, Val::Nil);
                if matches!(removed, Val::Nil) {
                    None
                } else {
                    self.dead_count += 1;
                    Some(removed)
                }
            }
        }
    }

    /// Stably remove logically absent slots. The caller owns version changes.
    fn compact_dead(&mut self) -> bool {
        if self.dead_count == 0 {
            return false;
        }
        match &mut self.storage {
            TableStorage::Inline { entries, len } => {
                let mut write = 0;
                let mut packed: [(Val, Val); INLINE_CAPACITY] = Default::default();
                let mut packed_slots = packed.iter_mut();
                for entry in entries.iter_mut().take(*len as usize) {
                    let entry = std::mem::take(entry);
                    if !matches!(entry.1, Val::Nil) {
                        *packed_slots
                            .next()
                            .expect("packed table must have room for every live entry") = entry;
                        write += 1;
                    }
                }
                *entries = packed;
                *len = write as u8;
            }
            TableStorage::Map(map) => map.retain(|_, value| !matches!(value, Val::Nil)),
        }
        self.dead_count = 0;
        true
    }

    /// Inserts a value at the given array position, shifting elements up.
    /// Position should be 1-based (Lua-style).
    #[hotpath::measure]
    pub(super) fn array_insert(&mut self, pos: usize, value: Val) {
        let len = self.array_len();
        let value_is_nil = matches!(value, Val::Nil);
        self.compact_dead();
        // For shift operations, ensure we're using Map storage
        self.ensure_map();
        if let TableStorage::Map(map) = &mut self.storage {
            // Shift elements from len down to pos up by one
            for i in (pos..=len).rev() {
                let key = Val::Num(i as f64);
                let next_key = Val::Num((i + 1) as f64);
                if let Some(v) = map.shift_remove(&key) {
                    map.insert(next_key, v);
                }
            }
        }
        self.bump_version();
        self.insert(Val::Num(pos as f64), value)
            .expect("array_insert: integer key insert cannot fail");
        if value_is_nil {
            self.cached_array_len.set(None);
        } else {
            self.cached_array_len.set(Some(len + 1));
        }
    }

    /// Removes and returns the value at the given array position, shifting elements down.
    /// Position should be 1-based (Lua-style).
    #[hotpath::measure]
    pub(super) fn array_remove(&mut self, pos: usize) -> Val {
        let len = self.array_len();
        if pos > len || pos == 0 {
            return Val::Nil;
        }
        self.compact_dead();
        // For shift operations, ensure we're using Map storage
        self.ensure_map();
        let removed = if let TableStorage::Map(map) = &mut self.storage {
            // Get the value to return
            let key = Val::Num(pos as f64);
            let removed = map.shift_remove(&key).unwrap_or(Val::Nil);
            // Shift elements down
            for i in pos..len {
                let next_key = Val::Num((i + 1) as f64);
                let curr_key = Val::Num(i as f64);
                if let Some(v) = map.shift_remove(&next_key) {
                    map.insert(curr_key, v);
                }
            }
            removed
        } else {
            Val::Nil
        };
        self.bump_version();
        // Update cache: new length is old length - 1
        self.cached_array_len.set(Some(len - 1));
        removed
    }

    /// Returns the array portion of the table as a Vec for sorting.
    /// Array indices are 1-based in Lua.
    #[hotpath::measure]
    pub(super) fn get_array(&self) -> Vec<Val> {
        let len = self.array_len();
        (1..=len)
            .map(|i| {
                let key = Val::Num(i as f64);
                self.get(&key)
            })
            .collect()
    }

    /// Replaces the array portion of the table with the given values.
    #[hotpath::measure]
    pub(super) fn set_array(&mut self, values: Vec<Val>) {
        // First remove old array elements
        let old_len = self.array_len();
        for i in 1..=old_len {
            self.remove(&Val::Num(i as f64));
        }
        // Insert new values and update cache directly (we know the new length)
        let new_len = values.len();
        for (i, v) in values.into_iter().enumerate() {
            // Use insert which handles both storage types
            self.insert(Val::Num((i + 1) as f64), v)
                .expect("set_array: integer key insert cannot fail");
        }
        self.cached_array_len.set(Some(new_len));
    }

    /// Returns the metatable of this table, if any.
    pub(super) fn get_metatable(&self) -> Option<ObjectPtr> {
        self.metatable
    }

    /// Sets the metatable of this table.
    pub(super) fn set_metatable(&mut self, mt: Option<ObjectPtr>) {
        self.metatable = mt;
    }

    /// Returns the next key-value pair after the given key.
    /// If key is nil, returns the first key-value pair. Distinguishes the end
    /// of iteration from an invalid control key.
    #[hotpath::measure]
    pub(super) fn next(&self, key: &Val) -> TableNext {
        if matches!(key, Val::Num(n) if n.is_nan()) {
            return TableNext::InvalidKey;
        }

        match &self.storage {
            TableStorage::Inline { entries, len } => {
                if matches!(key, Val::Nil) {
                    entries
                        .iter()
                        .take(*len as usize)
                        .find(|(_, value)| !matches!(value, Val::Nil))
                        .map_or(TableNext::End, |(k, v)| TableNext::Pair(*k, *v))
                } else {
                    // Find the key, then return the next one
                    match entries
                        .iter()
                        .take(*len as usize)
                        .position(|entry| &entry.0 == key)
                    {
                        Some(index) => entries
                            .iter()
                            .take(*len as usize)
                            .skip(index + 1)
                            .find(|(_, value)| !matches!(value, Val::Nil))
                            .map_or(TableNext::End, |(k, v)| TableNext::Pair(*k, *v)),
                        None => TableNext::InvalidKey,
                    }
                }
            }
            TableStorage::Map(map) => {
                if matches!(key, Val::Nil) {
                    if let Some((k, v)) = map.iter().find(|(_, value)| !matches!(value, Val::Nil)) {
                        TableNext::Pair(*k, *v)
                    } else {
                        TableNext::End
                    }
                } else {
                    match map.get_index_of(key) {
                        Some(index) => {
                            if let Some((k, v)) = map
                                .iter()
                                .skip(index + 1)
                                .find(|(_, value)| !matches!(value, Val::Nil))
                            {
                                TableNext::Pair(*k, *v)
                            } else {
                                TableNext::End
                            }
                        }
                        None => TableNext::InvalidKey,
                    }
                }
            }
        }
    }
}

impl Table {
    /// Mark all values contained in this table as reachable.
    /// Called by the GC during the mark phase.
    #[hotpath::measure]
    pub(super) fn mark_values(&self, heap: &GcHeap, upvalue_pool: &UpvaluePool) {
        match &self.storage {
            TableStorage::Inline { entries, len } => {
                for (key, value) in entries.iter().take(*len as usize) {
                    if !matches!(value, Val::Nil) {
                        key.mark_reachable(heap, upvalue_pool);
                        value.mark_reachable(heap, upvalue_pool);
                    }
                }
            }
            TableStorage::Map(map) => {
                for (k, v) in map {
                    if !matches!(v, Val::Nil) {
                        k.mark_reachable(heap, upvalue_pool);
                        v.mark_reachable(heap, upvalue_pool);
                    }
                }
            }
        }
        if let Some(mt) = &self.metatable {
            heap.mark(*mt, upvalue_pool);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn n(x: usize) -> Val {
        Val::Num(x as f64)
    }

    fn fill(t: &mut Table, range: std::ops::RangeInclusive<usize>) {
        for i in range {
            t.insert(n(i), Val::Bool(true)).unwrap();
        }
    }

    fn is_border(t: &Table, len: usize) -> bool {
        let after = matches!(t.get(&n(len + 1)), Val::Nil);
        let here = len == 0 || !matches!(t.get(&n(len)), Val::Nil);
        here && after
    }

    #[test]
    fn empty_table_has_border_zero() {
        let t = Table::default();
        assert_eq!(t.compute_array_len(), 0);
    }

    #[test]
    fn dense_inline_returns_exact_length() {
        // INLINE_CAPACITY entries, no holes.
        let mut t = Table::default();
        fill(&mut t, 1..=INLINE_CAPACITY);
        assert_eq!(t.compute_array_len(), INLINE_CAPACITY);
    }

    #[test]
    fn dense_map_returns_exact_length() {
        let mut t = Table::default();
        fill(&mut t, 1..=500);
        assert_eq!(t.compute_array_len(), 500);
    }

    #[test]
    fn cache_invalidated_on_insert() {
        let mut t = Table::default();
        fill(&mut t, 1..=10);
        assert_eq!(t.array_len(), 10);
        // Add an out-of-range key; current invalidation policy clears the cache.
        t.insert(n(20), Val::Bool(true)).unwrap();
        // Length is still a valid border (either 10 or 20).
        let len = t.array_len();
        assert!(is_border(&t, len), "len={len} is not a border");
    }

    #[test]
    fn dense_with_single_hole_returns_a_border() {
        // 1..=1000 with hole at 500. Borders are 499 and 1000.
        // Reference Lua 5.2/5.4 both return 1000 here.
        let mut t = Table::default();
        fill(&mut t, 1..=1000);
        t.insert(n(500), Val::Nil).unwrap();
        let len = t.compute_array_len();
        assert!(len == 499 || len == 1000, "len={len} is not a valid border");
        assert_eq!(len, 1000, "binary-search algorithm overshoots holes");
    }

    #[test]
    fn two_dense_runs_returns_a_border() {
        // 1..=3 dense, gap at 4, 5..=7 dense. Borders: 0, 3, 7.
        let mut t = Table::default();
        fill(&mut t, 1..=3);
        fill(&mut t, 5..=7);
        let len = t.compute_array_len();
        assert!(is_border(&t, len), "len={len} is not a border");
    }

    #[test]
    fn nil_at_one_returns_zero() {
        let mut t = Table::default();
        t.insert(n(2), Val::Bool(true)).unwrap();
        t.insert(n(3), Val::Bool(true)).unwrap();
        // t[1] is nil; the only valid border below 2 is 0.
        assert_eq!(t.compute_array_len(), 0);
    }

    #[test]
    fn single_element_returns_one() {
        let mut t = Table::default();
        t.insert(n(1), Val::Bool(true)).unwrap();
        assert_eq!(t.compute_array_len(), 1);
    }

    #[test]
    fn power_of_two_boundary() {
        // 1..=8 dense (the doubling search lands exactly on hi=16 → t[16] nil → bisect).
        let mut t = Table::default();
        fill(&mut t, 1..=8);
        assert_eq!(t.compute_array_len(), 8);
    }

    #[test]
    fn cache_returns_consistent_value() {
        let mut t = Table::default();
        fill(&mut t, 1..=64);
        let first = t.array_len();
        let second = t.array_len();
        assert_eq!(first, second);
        assert_eq!(first, 64);
    }

    #[test]
    fn next_distinguishes_end_from_invalid_key_inline() {
        let mut t = Table::default();
        fill(&mut t, 1..=3);

        assert!(matches!(
            t.next(&Val::Nil),
            TableNext::Pair(Val::Num(1.0), Val::Bool(true))
        ));
        assert!(matches!(
            t.next(&n(2)),
            TableNext::Pair(Val::Num(3.0), Val::Bool(true))
        ));
        assert!(matches!(t.next(&n(3)), TableNext::End));
        assert!(matches!(t.next(&n(4)), TableNext::InvalidKey));
        assert!(matches!(t.next(&Val::Num(f64::NAN)), TableNext::InvalidKey));
    }

    #[test]
    fn next_distinguishes_end_from_invalid_key_map() {
        let mut t = Table::default();
        fill(&mut t, 1..=5);

        assert!(matches!(
            t.next(&Val::Nil),
            TableNext::Pair(Val::Num(1.0), Val::Bool(true))
        ));
        assert!(matches!(
            t.next(&n(3)),
            TableNext::Pair(Val::Num(4.0), Val::Bool(true))
        ));
        assert!(matches!(t.next(&n(5)), TableNext::End));
        assert!(matches!(t.next(&n(6)), TableNext::InvalidKey));
        assert!(matches!(t.next(&Val::Num(f64::NAN)), TableNext::InvalidKey));
    }

    #[test]
    fn tombstones_advance_iteration_and_hide_indexed_access() {
        for count in [3, 6] {
            let mut t = Table::default();
            fill(&mut t, 1..=count);
            assert_eq!(t.version(), 0);
            assert_eq!(t.remove(&n(2)), Some(Val::Bool(true)));
            assert_eq!(t.version(), 0);
            assert!(matches!(t.next(&n(1)), TableNext::Pair(Val::Num(3.0), _)));
            assert!(matches!(t.next(&n(2)), TableNext::Pair(Val::Num(3.0), _)));
            assert!(matches!(t.next(&n(count + 1)), TableNext::InvalidKey));
            assert_eq!(t.get_with_index(&n(2)), None);
            assert_eq!(t.get_index(1), None);
            assert!(!t.set_at_index(1, Val::Bool(false)));
        }
    }

    #[test]
    fn reinserted_tombstone_moves_to_back_and_bumps_once() {
        for count in [3, 5] {
            let mut t = Table::default();
            fill(&mut t, 1..=count);
            t.remove(&n(2));
            t.insert(n(2), Val::Bool(false)).unwrap();
            assert_eq!(t.version(), 1);
            let mut control = Val::Nil;
            let mut keys = Vec::new();
            while let TableNext::Pair(key, _) = t.next(&control) {
                keys.push(key);
                control = key;
            }
            assert_eq!(
                keys,
                (1..=count)
                    .filter(|key| *key != 2)
                    .map(n)
                    .chain(std::iter::once(n(2)))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn dense_tombstones_compact_once_before_append() {
        let mut t = Table::default();
        fill(&mut t, 1..=4);
        t.remove(&n(2));
        t.remove(&n(3));
        assert!(matches!(t.next(&n(1)), TableNext::Pair(Val::Num(4.0), _)));
        t.insert(n(5), Val::Bool(true)).unwrap();
        assert_eq!(t.version(), 1);
        assert!(matches!(t.next(&n(4)), TableNext::Pair(Val::Num(5.0), _)));
    }
}
