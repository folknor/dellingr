use std::cell::Cell;

use indexmap::IndexMap;

use super::object::{GcHeap, Markable, ObjectPtr, UpvaluePool};
use super::Error;
use super::Result;
use super::TypeError;
use super::Val;

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
    /// Cached array length. None means cache is invalid and needs recomputation.
    /// Invalidated when positive integer keys are inserted or removed.
    /// Uses Cell for interior mutability so array_len() can cache on &self.
    cached_array_len: Cell<Option<usize>>,
}

impl Default for Table {
    fn default() -> Self {
        Self {
            storage: TableStorage::default(),
            metatable: None,
            cached_array_len: Cell::new(None),
        }
    }
}

impl Table {
    /// Check if a value is a positive integer (potential array index).
    #[inline]
    fn is_array_key(key: &Val) -> bool {
        if let Val::Num(n) = key {
            *n > 0.0 && n.is_finite() && *n == n.floor()
        } else {
            false
        }
    }

    pub(super) fn get(&self, key: &Val) -> Val {
        match key {
            Val::Nil => Val::Nil,
            Val::Num(n) if n.is_nan() => Val::Nil,
            _ => match &self.storage {
                TableStorage::Inline { entries, len } => {
                    for i in 0..(*len as usize) {
                        if &entries[i].0 == key {
                            return entries[i].1.clone();
                        }
                    }
                    Val::Nil
                }
                TableStorage::Map(map) => map.get(key).cloned().unwrap_or_default(),
            },
        }
    }

    /// Returns the "array length" of the table using standard Lua semantics.
    /// Counts consecutive integer keys starting from 1, stopping at the first nil.
    /// Uses cached value when available for O(1) performance.
    pub(super) fn array_len(&self) -> usize {
        if let Some(len) = self.cached_array_len.get() {
            return len;
        }
        let len = self.compute_array_len();
        self.cached_array_len.set(Some(len));
        len
    }

    /// Computes array length without caching (for internal use).
    fn compute_array_len(&self) -> usize {
        let mut len = 0;
        loop {
            let key = Val::Num((len + 1) as f64);
            let val = self.get(&key);
            if matches!(val, Val::Nil) {
                break;
            }
            len += 1;
        }
        len
    }

    pub(super) fn insert(&mut self, key: Val, value: Val) -> Result<()> {
        match &key {
            Val::Nil => return Err(Error::new(TypeError::TableKeyNil, 0, 0)),
            Val::Num(n) if n.is_nan() => return Err(Error::new(TypeError::TableKeyNan, 0, 0)),
            _ => {}
        }

        // Invalidate cache if this could affect array length
        if Self::is_array_key(&key) {
            self.cached_array_len.set(None);
        }

        match &mut self.storage {
            TableStorage::Inline { entries, len } => {
                // Check if key already exists (update in place)
                for i in 0..(*len as usize) {
                    if entries[i].0 == key {
                        entries[i].1 = value;
                        return Ok(());
                    }
                }
                // Key doesn't exist - need to add it
                if (*len as usize) < INLINE_CAPACITY {
                    // Still have room in inline storage
                    entries[*len as usize] = (key, value);
                    *len += 1;
                } else {
                    // Need to promote to Map
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
    fn promote_to_map(&mut self, new_key: Val, new_value: Val) {
        let old_storage = std::mem::take(&mut self.storage);
        if let TableStorage::Inline { mut entries, len } = old_storage {
            let mut map = IndexMap::with_capacity(INLINE_CAPACITY + 1);
            for i in 0..(len as usize) {
                let (k, v) = std::mem::take(&mut entries[i]);
                map.insert(k, v);
            }
            map.insert(new_key, new_value);
            self.storage = TableStorage::Map(map);
        }
    }

    /// Ensure storage is Map (for operations that need IndexMap's shift_remove).
    fn ensure_map(&mut self) {
        // Only convert if currently Inline
        if matches!(self.storage, TableStorage::Inline { .. }) {
            if let TableStorage::Inline { mut entries, len } =
                std::mem::take(&mut self.storage)
            {
                let mut map = IndexMap::with_capacity(len as usize);
                for i in 0..(len as usize) {
                    let (k, v) = std::mem::take(&mut entries[i]);
                    map.insert(k, v);
                }
                self.storage = TableStorage::Map(map);
            }
        }
    }

    /// Remove a key and return its value (if any).
    fn remove(&mut self, key: &Val) -> Option<Val> {
        match &mut self.storage {
            TableStorage::Inline { entries, len } => {
                for i in 0..(*len as usize) {
                    if &entries[i].0 == key {
                        let removed = std::mem::take(&mut entries[i].1);
                        // Shift remaining entries down
                        for j in i..(*len as usize - 1) {
                            entries[j] = std::mem::take(&mut entries[j + 1]);
                        }
                        *len -= 1;
                        return Some(removed);
                    }
                }
                None
            }
            TableStorage::Map(map) => map.shift_remove(key),
        }
    }

    /// Inserts a value at the given array position, shifting elements up.
    /// Position should be 1-based (Lua-style).
    pub(super) fn array_insert(&mut self, pos: usize, value: Val) {
        let len = self.array_len();
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
            // Insert the new value at pos
            map.insert(Val::Num(pos as f64), value);
        }
        // Update cache: new length is old length + 1
        self.cached_array_len.set(Some(len + 1));
    }

    /// Removes and returns the value at the given array position, shifting elements down.
    /// Position should be 1-based (Lua-style).
    pub(super) fn array_remove(&mut self, pos: usize) -> Val {
        let len = self.array_len();
        if pos > len || pos == 0 {
            return Val::Nil;
        }
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
        // Update cache: new length is old length - 1
        self.cached_array_len.set(Some(len - 1));
        removed
    }

    /// Returns the array portion of the table as a Vec for sorting.
    /// Array indices are 1-based in Lua.
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
            let _ = self.insert(Val::Num((i + 1) as f64), v);
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
    /// If key is nil, returns the first key-value pair.
    /// Returns (nil, nil) when there are no more pairs.
    pub(super) fn next(&self, key: &Val) -> (Val, Val) {
        match &self.storage {
            TableStorage::Inline { entries, len } => {
                if matches!(key, Val::Nil) {
                    // Return the first key-value pair
                    if *len > 0 {
                        return (entries[0].0.clone(), entries[0].1.clone());
                    }
                } else {
                    // Find the key, then return the next one
                    for i in 0..(*len as usize) {
                        if &entries[i].0 == key {
                            if i + 1 < *len as usize {
                                return (entries[i + 1].0.clone(), entries[i + 1].1.clone());
                            }
                            break;
                        }
                    }
                }
            }
            TableStorage::Map(map) => {
                if matches!(key, Val::Nil) {
                    // Return the first key-value pair
                    if let Some((k, v)) = map.iter().next() {
                        return (k.clone(), v.clone());
                    }
                } else {
                    // Find the key, then return the next one
                    let mut found = false;
                    for (k, v) in map {
                        if found {
                            return (k.clone(), v.clone());
                        }
                        if k == key {
                            found = true;
                        }
                    }
                }
            }
        }
        (Val::Nil, Val::Nil)
    }
}

impl Table {
    /// Mark all values contained in this table as reachable.
    /// Called by the GC during the mark phase.
    pub(super) fn mark_values(&self, heap: &GcHeap, upvalue_pool: &UpvaluePool) {
        match &self.storage {
            TableStorage::Inline { entries, len } => {
                for i in 0..(*len as usize) {
                    entries[i].0.mark_reachable(heap, upvalue_pool);
                    entries[i].1.mark_reachable(heap, upvalue_pool);
                }
            }
            TableStorage::Map(map) => {
                for (k, v) in map {
                    k.mark_reachable(heap, upvalue_pool);
                    v.mark_reachable(heap, upvalue_pool);
                }
            }
        }
        if let Some(mt) = &self.metatable {
            heap.mark(*mt, upvalue_pool);
        }
    }
}
