use std::collections::HashMap;

use super::Error;
use super::Markable;
use super::Result;
use super::TypeError;
use super::Val;

#[derive(Debug, Default)]
pub(super) struct Table {
    map: HashMap<Val, Val>,
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
}

impl Markable for Table {
    fn mark_reachable(&self) {
        for (k, v) in &self.map {
            k.mark_reachable();
            v.mark_reachable();
        }
    }
}
