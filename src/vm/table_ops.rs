//! Table operations for the Lua VM.
//!
//! This module contains methods for creating and manipulating Lua tables,
//! including metamethod-aware operations.

use super::lua_val::Val;
use super::object::Markable;
use super::{Result, State, TypeError};
use crate::instr::{ArgCount, RetCount};

impl State {
    /// Creates a new empty table and pushes it onto the stack.
    pub fn new_table(&mut self) {
        let val = self.alloc_table();
        self.stack.push(val);
    }

    /// Pushes onto the stack the value `t[k]`, where `t` is the value at the given
    /// valid index and `k` is the value at the top of the stack.
    ///
    /// This function pops the key from the stack (putting the resulting value in
    /// its place). As in Lua, this function may trigger a metamethod for the
    /// "index" event.
    pub fn get_table(&mut self, i: isize) -> Result<()> {
        let idx = self.convert_idx(i)?;
        assert!(idx != self.stack.len() - 1);
        let key = self.pop_val();
        self.get_table_with_key(idx, key)
    }

    /// Gets `t[k]` without invoking metamethods.
    /// `t` is at the given index, `k` is at the top of the stack.
    /// Pops the key and pushes the result.
    pub fn get_table_raw(&mut self, i: isize) -> Result<()> {
        let idx = self.convert_idx(i)?;
        let key = self.pop_val();
        let table = &self.stack[idx];
        let typ = table.typ();
        match table.as_table_ref() {
            Some(t) => {
                let val = t.get(&key);
                self.stack.push(val);
                Ok(())
            }
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    /// Does the equivalent of `t[k] = v`, where `t` is the value at the given
    /// valid index, `k` is the value at the top of the stack minus 1, and `v`
    /// is the value at the top of the stack.
    ///
    /// This function pops both the key and the value from the stack.
    pub fn set_table_raw(&mut self, i: isize) -> Result<()> {
        let idx = self.convert_idx(i)?;
        let key = self.pop_val();
        let val = self.pop_val();
        let table = &mut self.stack[idx];
        let typ = table.typ();
        match table.as_table() {
            Some(t) => {
                t.insert(key, val)?;
                Ok(())
            }
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    /// Returns the next key-value pair from a table, for use with `pairs`.
    /// Takes the table index and pops the key from the stack.
    /// Pushes the next key and value onto the stack (or just nil if done).
    pub fn table_next(&mut self, table_idx: isize) -> Result<bool> {
        let idx = self.convert_idx(table_idx)?;
        let key = self.pop_val();
        let table_val = &self.stack[idx];
        match table_val.as_table_ref() {
            Some(t) => {
                let (next_key, next_val) = t.next(&key);
                if matches!(next_key, Val::Nil) {
                    self.stack.push(Val::Nil);
                    Ok(false)
                } else {
                    self.stack.push(next_key);
                    self.stack.push(next_val);
                    Ok(true)
                }
            }
            None => Err(self.type_error(TypeError::TableIndex(table_val.typ()))),
        }
    }

    /// Returns the array length of the table at the given index.
    pub fn table_len(&self, idx: isize) -> usize {
        let i = match self.convert_idx(idx) {
            Ok(i) => i,
            Err(_) => return 0,
        };
        let table_val = &self.stack[i];
        match table_val.as_table_ref() {
            Some(t) => t.array_len(),
            None => 0,
        }
    }

    /// Gets the metatable of the value at the given index.
    /// For tables, returns the table's metatable.
    /// For other types, returns nil (we don't support type metatables yet).
    pub fn get_metatable_of(&mut self, idx: isize) -> Result<()> {
        let i = self.convert_idx(idx)?;
        let val = &self.stack[i];
        match val.as_table_ref() {
            Some(t) => {
                if let Some(mt) = t.get_metatable() {
                    self.stack.push(Val::Obj(mt));
                } else {
                    self.push_nil();
                }
            }
            None => self.push_nil(),
        }
        Ok(())
    }

    /// Sets the metatable of the table at the given index.
    /// The metatable should be at the top of the stack (or nil to remove).
    /// Pops the metatable from the stack.
    pub fn set_metatable_of(&mut self, table_idx: isize) -> Result<()> {
        let mt_val = self.pop_val();
        let idx = self.convert_idx(table_idx)?;
        let typ = self.stack[idx].typ();

        let mt = match &mt_val {
            Val::Nil => None,
            Val::Obj(ptr) => {
                if ptr.as_table_ref().is_some() {
                    Some(*ptr)
                } else {
                    return Err(self.type_error(TypeError::TableIndex(mt_val.typ())));
                }
            }
            _ => return Err(self.type_error(TypeError::TableIndex(mt_val.typ()))),
        };

        match self.stack[idx].as_table() {
            Some(t) => {
                t.set_metatable(mt);
                Ok(())
            }
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    /// Inserts a value into a table at a position, shifting elements.
    /// Stack: [t, pos, value] -> []
    /// The pos and value are popped from the stack.
    pub fn table_insert_at(&mut self, table_idx: isize) -> Result<()> {
        let value = self.pop_val();
        let pos = self.pop_val().as_num().unwrap_or(1.0) as usize;
        let idx = self.convert_idx(table_idx)?;
        let typ = self.stack[idx].typ();
        match self.stack[idx].as_table() {
            Some(t) => {
                t.array_insert(pos, value);
                Ok(())
            }
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    /// Removes a value from a table at a position, shifting elements.
    /// Pushes the removed value onto the stack.
    pub fn table_remove_at(&mut self, table_idx: isize, pos: usize) -> Result<()> {
        let idx = self.convert_idx(table_idx)?;
        let typ = self.stack[idx].typ();
        let removed = match self.stack[idx].as_table() {
            Some(t) => t.array_remove(pos),
            None => return Err(self.type_error(TypeError::TableIndex(typ))),
        };
        self.stack.push(removed);
        Ok(())
    }

    /// Sorts the array portion of a table in place.
    /// If has_comp is true, uses the function at stack index 2 as comparator.
    pub fn table_sort(&mut self, table_idx: isize, has_comp: bool) -> Result<()> {
        let idx = self.convert_idx(table_idx)?;

        // Get the array values
        let mut arr = {
            let table_val = &self.stack[idx];
            match table_val.as_table_ref() {
                Some(t) => t.get_array(),
                None => return Err(self.type_error(TypeError::TableIndex(table_val.typ()))),
            }
        };

        if arr.is_empty() {
            return Ok(());
        }

        if has_comp {
            // Use the comparator function at stack index 2
            // We need to do a stable sort with the comparator
            let comp_idx = self.convert_idx(2)?;

            // Bubble sort to keep it simple (not efficient but works)
            let n = arr.len();
            for i in 0..n {
                for j in 0..n - 1 - i {
                    // Call comp(arr[j], arr[j+1])
                    let a = arr[j].clone();
                    let b = arr[j + 1].clone();

                    // Push comp function
                    self.stack.push(self.stack[comp_idx].clone());
                    // Push args
                    self.stack.push(a.clone());
                    self.stack.push(b.clone());
                    // Call
                    self.call(ArgCount::Fixed(2), RetCount::Fixed(1))?;
                    // Get result
                    let result = self.pop_val();

                    // If comp(a, b) is false, swap
                    if !result.truthy() {
                        arr.swap(j, j + 1);
                    }
                }
            }
        } else {
            // Default: sort by < operator (numbers first, then strings)
            arr.sort_by(|a, b| {
                match (a.as_num(), b.as_num()) {
                    (Some(na), Some(nb)) => na.partial_cmp(&nb).unwrap_or(std::cmp::Ordering::Equal),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => {
                        // Try string comparison
                        match (a.as_string(), b.as_string()) {
                            (Some(sa), Some(sb)) => sa.cmp(sb),
                            _ => std::cmp::Ordering::Equal,
                        }
                    }
                }
            });
        }

        // Put sorted array back
        let typ = self.stack[idx].typ();
        match self.stack[idx].as_table() {
            Some(t) => {
                t.set_array(arr);
                Ok(())
            }
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    /// Converts the value at the given index to a string, checking for __tostring metamethod.
    /// If the value is a table with a __tostring metamethod, calls it and returns the result.
    pub fn to_string_with_meta(&mut self, idx: isize) -> Result<String> {
        let i = self.convert_idx(idx)?;
        let val = self.stack[i].clone();

        // Check if it's a table with __tostring
        if let Some(t) = val.as_table_ref() {
            if let Some(mt_ptr) = t.get_metatable() {
                let tostring_key = self.alloc_string("__tostring".to_string());
                if let Some(mt) = Val::Obj(mt_ptr).as_table() {
                    let tostring_handler = mt.get(&tostring_key);
                    if !matches!(tostring_handler, Val::Nil) {
                        // Call the __tostring metamethod
                        self.stack.push(tostring_handler);
                        self.stack.push(val);
                        self.call(ArgCount::Fixed(1), RetCount::Fixed(1))?;
                        let result = self.pop_val();
                        return Ok(result.to_string());
                    }
                }
            }
        }

        // No __tostring, use default
        Ok(val.to_string())
    }

    /// Allocates a new table on the heap.
    pub(super) fn alloc_table(&mut self) -> Val {
        let Self {
            stack,
            globals,
            string_literals,
            ..
        } = self;
        let obj = self.heap.new_table(|| {
            stack.mark_reachable();
            globals.mark_reachable();
            string_literals.mark_reachable();
        });
        Val::Obj(obj)
    }
}
