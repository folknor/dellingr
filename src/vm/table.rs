use indexmap::IndexMap;

use super::object::ObjectPtr;
use super::Error;
use super::Markable;
use super::Result;
use super::TypeError;
use super::Val;

/// A Lua table using IndexMap to maintain insertion order.
/// This ensures deterministic iteration order with `pairs()`.
#[derive(Debug, Default)]
pub(super) struct Table {
    map: IndexMap<Val, Val>,
    metatable: Option<ObjectPtr>,
}

impl Table {
    pub(super) fn get(&self, key: &Val) -> Val {
        match key {
            Val::Nil => Val::Nil,
            Val::Num(n) if n.is_nan() => Val::Nil,
            _ => self.map.get(key).cloned().unwrap_or_default(),
        }
    }

    /// Returns the "array length" of the table using standard Lua semantics.
    /// Counts consecutive integer keys starting from 1, stopping at the first nil.
    pub(super) fn array_len(&self) -> usize {
        let mut len = 0;
        loop {
            let key = Val::Num((len + 1) as f64);
            match self.map.get(&key) {
                Some(val) if !matches!(val, Val::Nil) => len += 1,
                _ => break,
            }
        }
        len
    }

    pub(super) fn insert(&mut self, key: Val, value: Val) -> Result<()> {
        match key {
            Val::Nil => Err(Error::new(TypeError::TableKeyNil, 0, 0)),
            Val::Num(n) if n.is_nan() => Err(Error::new(TypeError::TableKeyNan, 0, 0)),
            _ => {
                self.map.insert(key, value);
                Ok(())
            }
        }
    }

    /// Inserts a value at the given array position, shifting elements up.
    /// Position should be 1-based (Lua-style).
    pub(super) fn array_insert(&mut self, pos: usize, value: Val) {
        let len = self.array_len();
        // Shift elements from len down to pos up by one
        for i in (pos..=len).rev() {
            let key = Val::Num(i as f64);
            let next_key = Val::Num((i + 1) as f64);
            if let Some(v) = self.map.shift_remove(&key) {
                self.map.insert(next_key, v);
            }
        }
        // Insert the new value at pos
        self.map.insert(Val::Num(pos as f64), value);
    }

    /// Removes and returns the value at the given array position, shifting elements down.
    /// Position should be 1-based (Lua-style).
    pub(super) fn array_remove(&mut self, pos: usize) -> Val {
        let len = self.array_len();
        if pos > len || pos == 0 {
            return Val::Nil;
        }
        // Get the value to return
        let key = Val::Num(pos as f64);
        let removed = self.map.shift_remove(&key).unwrap_or(Val::Nil);
        // Shift elements down
        for i in pos..len {
            let next_key = Val::Num((i + 1) as f64);
            let curr_key = Val::Num(i as f64);
            if let Some(v) = self.map.shift_remove(&next_key) {
                self.map.insert(curr_key, v);
            }
        }
        removed
    }

    /// Returns the array portion of the table as a Vec for sorting.
    /// Array indices are 1-based in Lua.
    pub(super) fn get_array(&self) -> Vec<Val> {
        let len = self.array_len();
        (1..=len)
            .map(|i| {
                let key = Val::Num(i as f64);
                self.map.get(&key).cloned().unwrap_or(Val::Nil)
            })
            .collect()
    }

    /// Replaces the array portion of the table with the given values.
    pub(super) fn set_array(&mut self, values: Vec<Val>) {
        // First remove old array elements
        let old_len = self.array_len();
        for i in 1..=old_len {
            self.map.shift_remove(&Val::Num(i as f64));
        }
        // Insert new values
        for (i, v) in values.into_iter().enumerate() {
            self.map.insert(Val::Num((i + 1) as f64), v);
        }
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
        if matches!(key, Val::Nil) {
            // Return the first key-value pair
            if let Some((k, v)) = self.map.iter().next() {
                return (k.clone(), v.clone());
            }
        } else {
            // Find the key, then return the next one
            let mut found = false;
            for (k, v) in &self.map {
                if found {
                    return (k.clone(), v.clone());
                }
                if k == key {
                    found = true;
                }
            }
        }
        (Val::Nil, Val::Nil)
    }
}

impl Markable for Table {
    fn mark_reachable(&self) {
        for (k, v) in &self.map {
            k.mark_reachable();
            v.mark_reachable();
        }
        if let Some(mt) = &self.metatable {
            mt.mark_reachable();
        }
    }
}
