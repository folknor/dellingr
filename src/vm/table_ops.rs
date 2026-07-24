//! Table operations for the Lua VM.
//!
//! This module contains methods for creating and manipulating Lua tables,
//! including metamethod-aware operations.

use super::lua_val::RustFunc;
use super::lua_val::Val;
use super::{Result, State, TypeError};
#[cfg(feature = "snapshot")]
use crate::error::ErrorKind;
use crate::instr::{ArgCount, RetCount};

impl State {
    /// Creates a new empty table and pushes it onto the stack.
    #[hotpath::measure]
    pub fn new_table(&mut self) {
        let val = self.alloc_table();
        self.stack.push(val);
    }

    pub(crate) fn new_table_with_capacity(&mut self, capacity: usize) {
        let val = self.alloc_table_with_capacity(capacity);
        self.stack.push(val);
    }

    pub(super) fn new_table_with_template(&mut self, key_ids: &[u8], string_literal_start: usize) {
        if self.heap.is_full() {
            self.gc_collect();
        }
        let obj = self.heap.alloc_table_with_template(
            key_ids,
            &self.string_literals,
            string_literal_start,
        );
        self.stack.push(Val::Obj(obj));
    }

    pub(crate) fn set_table_str_key_value(
        &mut self,
        table_idx: isize,
        name: &str,
        val: Val,
    ) -> Result<()> {
        let key = self.alloc_string(name);
        let idx = self.convert_idx(table_idx)?;
        let obj_ptr = self.stack[idx].as_object_ptr();
        let typ = self.stack[idx].typ_simple();

        match obj_ptr.and_then(|ptr| self.heap.as_table(ptr)) {
            Some(t) => {
                t.insert(key, val)?;
                Ok(())
            }
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    #[cfg(not(feature = "snapshot"))]
    pub(crate) fn set_table_str_key_rust_fn(
        &mut self,
        table_idx: isize,
        name: &str,
        func: RustFunc,
    ) -> Result<()> {
        self.set_table_str_key_value(table_idx, name, Val::RustFn(func))
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn set_table_str_key_named_rust_fn(
        &mut self,
        table_idx: isize,
        name: &str,
        id: &str,
        func: RustFunc,
    ) -> Result<()> {
        self.register_rust_fn(id, func)
            .map_err(|err| self.error(ErrorKind::InternalError(err.to_string())))?;
        self.set_table_str_key_value(table_idx, name, Val::RustFn(func))
    }

    pub(crate) fn set_table_str_key_number(
        &mut self,
        table_idx: isize,
        name: &str,
        num: f64,
    ) -> Result<()> {
        self.set_table_str_key_value(table_idx, name, Val::Num(num))
    }

    /// Pushes onto the stack the value `t[k]`, where `t` is the value at the given
    /// valid index and `k` is the value at the top of the stack.
    ///
    /// This function pops the key from the stack (putting the resulting value in
    /// its place). As in Lua, this function may trigger a metamethod for the
    /// "index" event.
    #[hotpath::measure]
    pub fn get_table(&mut self, i: isize) -> Result<()> {
        let idx = self.convert_idx(i)?;
        assert!(idx != self.stack.len() - 1);
        let key = self.pop_val();
        let mut local_cost = 0;
        self.get_table_with_key(idx, key, &mut local_cost)
    }

    /// Gets `t[k]` without invoking metamethods.
    /// `t` is at the given index, `k` is at the top of the stack.
    /// Pops the key and pushes the result.
    #[hotpath::measure]
    pub fn get_table_raw(&mut self, i: isize) -> Result<()> {
        let idx = self.convert_idx(i)?;
        let key = self.pop_val();

        // Get the ObjectPtr and type for error reporting
        let obj_ptr = self.stack[idx].as_object_ptr();
        let typ = self.stack[idx].typ_simple();

        match obj_ptr.and_then(|ptr| self.heap.as_table_ref(ptr)) {
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
    /// This function pops both the key and the value from the stack. Matches
    /// the reference Lua C API's `lua_rawset`.
    #[hotpath::measure]
    pub fn set_table_raw(&mut self, i: isize) -> Result<()> {
        let idx = self.convert_idx(i)?;
        let val = self.pop_val();
        let key = self.pop_val();

        // Get the ObjectPtr and type for error reporting
        let obj_ptr = self.stack[idx].as_object_ptr();
        let typ = self.stack[idx].typ_simple();

        match obj_ptr.and_then(|ptr| self.heap.as_table(ptr)) {
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
    #[hotpath::measure]
    pub fn table_next(&mut self, table_idx: isize) -> Result<bool> {
        let idx = self.convert_idx(table_idx)?;
        let key = self.pop_val();

        let obj_ptr = self.stack[idx].as_object_ptr();
        let typ = self.stack[idx].typ_simple();

        match obj_ptr.and_then(|ptr| self.heap.as_table_ref(ptr)) {
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
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    /// Returns the array length of the table at the given index.
    #[hotpath::measure]
    pub fn table_len(&self, idx: isize) -> usize {
        let i = match self.convert_idx(idx) {
            Ok(i) => i,
            Err(_) => return 0,
        };
        self.stack[i]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .map_or(0, super::table::Table::array_len)
    }

    /// Gets the metatable of the value at the given index.
    /// For tables, returns the table's metatable.
    /// For other types, returns nil (we don't support type metatables yet).
    #[hotpath::measure]
    pub fn get_metatable_of(&mut self, idx: isize) -> Result<()> {
        let i = self.convert_idx(idx)?;

        let metatable = self.stack[i]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .and_then(super::table::Table::get_metatable);

        match metatable {
            Some(mt) => self.stack.push(Val::Obj(mt)),
            None => self.push_nil(),
        }
        Ok(())
    }

    /// Sets the metatable of the table at the given index.
    /// The metatable should be at the top of the stack (or nil to remove).
    /// Pops the metatable from the stack.
    #[hotpath::measure]
    pub fn set_metatable_of(&mut self, table_idx: isize) -> Result<()> {
        let mt_val = self.pop_val();
        let idx = self.convert_idx(table_idx)?;
        let typ = self.stack[idx].typ_simple();

        let mt = match mt_val {
            Val::Nil => None,
            Val::Obj(ptr) if self.heap.as_table_ref(ptr).is_some() => Some(ptr),
            other => return Err(self.type_error(TypeError::TableIndex(other.typ_simple()))),
        };

        match self.stack[idx]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table(ptr))
        {
            Some(t) => {
                t.set_metatable(mt);
                Ok(())
            }
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    /// Inserts a value into a table at a position, shifting elements.
    /// Stack: [t, value] -> []
    /// The value is popped from the stack. `pos` has been validated by table.insert.
    #[hotpath::measure]
    pub fn table_insert_at(&mut self, table_idx: isize, pos: usize) -> Result<()> {
        let value = self.pop_val();
        let idx = self.convert_idx(table_idx)?;
        let obj_ptr = self.stack[idx].as_object_ptr();
        let typ = self.stack[idx].typ_simple();

        match obj_ptr.and_then(|ptr| self.heap.as_table(ptr)) {
            Some(t) => {
                t.array_insert(pos, value);
                Ok(())
            }
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    /// Removes a value from a table at a position, shifting elements.
    /// Pushes the removed value onto the stack.
    #[hotpath::measure]
    pub fn table_remove_at(&mut self, table_idx: isize, pos: usize) -> Result<()> {
        let idx = self.convert_idx(table_idx)?;
        let obj_ptr = self.stack[idx].as_object_ptr();
        let typ = self.stack[idx].typ_simple();

        let removed = match obj_ptr.and_then(|ptr| self.heap.as_table(ptr)) {
            Some(t) => t.array_remove(pos),
            None => return Err(self.type_error(TypeError::TableIndex(typ))),
        };
        self.stack.push(removed);
        Ok(())
    }

    /// Sorts the array portion of a table in place.
    /// If has_comp is true, uses the function at stack index 2 as comparator.
    /// Sort a table's array portion. Returns the array length for cost charging.
    #[hotpath::measure]
    pub fn table_sort(&mut self, table_idx: isize, has_comp: bool) -> Result<usize> {
        let idx = self.convert_idx(table_idx)?;

        // Get the array values
        let obj_ptr = self.stack[idx].as_object_ptr();
        let typ = self.stack[idx].typ_simple();

        let mut arr = match obj_ptr.and_then(|ptr| self.heap.as_table_ref(ptr)) {
            Some(t) => t.get_array(),
            None => return Err(self.type_error(TypeError::TableIndex(typ))),
        };

        // Charge cost BEFORE running the comparator or mutating the table, so an
        // exhausted budget blocks the sort rather than letting it complete and
        // only then failing the charge (L18). `arr` is a detached copy here, so
        // nothing has been mutated yet. Empty sorts still cost 1.
        let n = arr.len();
        self.consume_cost(n.max(1) as u64)?;

        if arr.is_empty() {
            return Ok(0);
        }

        if has_comp {
            // Use the comparator function at stack index 2
            // We need to do a stable sort with the comparator
            let comp_idx = self.convert_idx(2)?;

            // Bubble sort to keep it simple (not efficient but works)
            for i in 0..n {
                for j in 0..n - 1 - i {
                    // Call comp(arr[j], arr[j+1])
                    let a = arr[j];
                    let b = arr[j + 1];

                    // Push comp function
                    self.stack.push(self.stack[comp_idx]);
                    // Push args
                    self.stack.push(a);
                    self.stack.push(b);
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
            let heap = &self.heap;
            arr.sort_by(|a, b| {
                match (a.as_num(), b.as_num()) {
                    (Some(na), Some(nb)) => {
                        na.partial_cmp(&nb).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => {
                        // Try string comparison
                        match (a.as_string(heap), b.as_string(heap)) {
                            (Some(sa), Some(sb)) => sa.cmp(sb),
                            _ => std::cmp::Ordering::Equal,
                        }
                    }
                }
            });
        }

        // Put sorted array back - need to look up the table again since we may have
        // mutated self during comparator calls
        let obj_ptr = self.stack[idx].as_object_ptr();
        match obj_ptr.and_then(|ptr| self.heap.as_table(ptr)) {
            Some(t) => {
                t.set_array(arr);
                Ok(n)
            }
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    /// Converts the value at the given index to a string, checking for __tostring metamethod.
    /// If the value is a table with a __tostring metamethod, calls it and returns the result.
    #[hotpath::measure]
    pub fn to_string_with_meta(&mut self, idx: isize) -> Result<String> {
        let i = self.convert_idx(idx)?;
        let val = self.stack[i];

        // Check if it's a table with __tostring
        let metatable_ptr = val
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .and_then(super::table::Table::get_metatable);

        if let Some(mt_ptr) = metatable_ptr {
            let tostring_key = self.alloc_string("__tostring");
            let tostring_handler = self
                .heap
                .as_table_ref(mt_ptr)
                .map_or(Val::Nil, |mt| mt.get(&tostring_key));

            if !matches!(tostring_handler, Val::Nil) {
                // Call the __tostring metamethod
                self.stack.push(tostring_handler);
                self.stack.push(val);
                self.call(ArgCount::Fixed(1), RetCount::Fixed(1))?;
                let result = self.pop_val();
                return Ok(result.to_string_with_heap(&self.heap));
            }
        }

        // No __tostring, use default
        Ok(val.to_string_with_heap(&self.heap))
    }

    /// Allocates a new table on the heap.
    #[hotpath::measure]
    pub(super) fn alloc_table(&mut self) -> Val {
        // Check if GC is needed before allocating
        if self.heap.is_full() {
            self.gc_collect();
        }
        let obj = self.heap.alloc_table();
        Val::Obj(obj)
    }

    pub(super) fn alloc_table_with_capacity(&mut self, capacity: usize) -> Val {
        if self.heap.is_full() {
            self.gc_collect();
        }
        let obj = self.heap.alloc_table_with_capacity(capacity);
        Val::Obj(obj)
    }
}
