//! Table operations for the Lua VM.
//!
//! This module contains methods for creating and manipulating Lua tables,
//! including metamethod-aware operations.

use super::lua_val::RustFunc;
use super::lua_val::Val;
use super::table::TableNext;
use super::{Result, State, TypeError};
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
        self.with_rooted_value(val, |state| {
            let key = state.alloc_string(name);
            let idx = state.convert_idx(table_idx)?;
            let obj_ptr = state.stack[idx].as_object_ptr();
            let typ = state.stack[idx].typ(&state.heap);

            match obj_ptr.and_then(|ptr| state.heap.as_table(ptr)) {
                Some(t) => {
                    t.insert(key, val)?;
                    Ok(())
                }
                None => Err(state.type_error(TypeError::TableIndex(typ))),
            }
        })
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
        let typ = self.stack[idx].typ(&self.heap);

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
        // Both operands must sit ABOVE the table, so that popping them leaves
        // the table itself in place for the lookup below. A host calling this
        // with too few visible values is a mistake, not a VM bug, so it must
        // produce an error rather than reach the panicking `pop_val` - or pop
        // the table and then index off the end of the stack.
        if self.stack.len() < idx + 3 {
            return Err(self.error(ErrorKind::InvalidStackIndex { index: -2 }));
        }
        let val = self.pop_val();
        let key = self.pop_val();

        // Get the ObjectPtr and type for error reporting
        let obj_ptr = self.stack[idx].as_object_ptr();
        let typ = self.stack[idx].typ(&self.heap);

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
        let typ = self.stack[idx].typ(&self.heap);

        match obj_ptr.and_then(|ptr| self.heap.as_table_ref(ptr)) {
            Some(t) => match t.next(&key) {
                TableNext::Pair(next_key, next_val) => {
                    self.stack.push(next_key);
                    self.stack.push(next_val);
                    Ok(true)
                }
                TableNext::End => {
                    self.stack.push(Val::Nil);
                    Ok(false)
                }
                TableNext::InvalidKey => {
                    Err(self.error(ErrorKind::RuntimeError("invalid key to 'next'".into())))
                }
            },
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
        // Validate before popping, so a bad table index or an empty stack leaves
        // the stack untouched instead of consuming a value or panicking in
        // `pop_val`.
        let idx = self.convert_idx(table_idx)?;
        // The metatable must sit above the table, for the same reason.
        if self.stack.len() < idx + 2 {
            return Err(self.error(ErrorKind::InvalidStackIndex { index: -1 }));
        }
        let mt_val = self.pop_val();
        let typ = self.stack[idx].typ(&self.heap);

        let mt = match mt_val {
            Val::Nil => None,
            Val::Obj(ptr) if self.heap.as_table_ref(ptr).is_some() => Some(ptr),
            other => return Err(self.type_error(TypeError::TableIndex(other.typ(&self.heap)))),
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
        let typ = self.stack[idx].typ(&self.heap);

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
        let typ = self.stack[idx].typ(&self.heap);

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
        let typ = self.stack[idx].typ(&self.heap);

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

        let comp_idx = has_comp.then(|| self.convert_idx(2)).transpose()?;

        // Heap sort remains bounded and deterministic even when a Lua
        // comparator is inconsistent. Unlike reference quicksort, it does not
        // diagnose that incidental invalid-order case.
        let sort = |state: &mut Self, arr: &mut Vec<Val>| -> Result<()> {
            for root in (0..n / 2).rev() {
                state.table_sort_sift_down(arr, root, n, comp_idx)?;
            }
            for end in (1..n).rev() {
                arr.swap(0, end);
                state.table_sort_sift_down(arr, 0, end, comp_idx)?;
            }
            arr.reverse();
            Ok(())
        };

        match comp_idx {
            // A comparator can re-enter Lua and force a collection, and `arr` is
            // a detached `Vec<Val>` that GC cannot see, so the whole array has
            // to stay rooted for the entire sort - not just the pair being
            // compared. `src/vm/tests.rs` covers a comparator that clears the
            // source table before collecting.
            Some(_) => {
                let roots = arr.clone();
                self.with_rooted_values(&roots, |state| sort(state, &mut arr))?;
            }
            // The default comparison only reads numbers and already-interned
            // string bytes: it cannot call Lua, allocate, or trigger GC, and the
            // source table holds every element until writeback. Rooting here
            // would just copy the array twice more for nothing.
            None => sort(self, &mut arr)?,
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

    fn table_sort_sift_down(
        &mut self,
        arr: &mut [Val],
        mut root: usize,
        end: usize,
        comp_idx: Option<usize>,
    ) -> Result<()> {
        loop {
            let left = root * 2 + 1;
            if left >= end {
                return Ok(());
            }
            let mut smallest = root;
            if self.table_sort_less(arr[left], arr[smallest], comp_idx)? {
                smallest = left;
            }
            let right = left + 1;
            if right < end && self.table_sort_less(arr[right], arr[smallest], comp_idx)? {
                smallest = right;
            }
            if smallest == root {
                return Ok(());
            }
            arr.swap(root, smallest);
            root = smallest;
        }
    }

    fn table_sort_less(&mut self, a: Val, b: Val, comp_idx: Option<usize>) -> Result<bool> {
        if let Some(comp_idx) = comp_idx {
            self.stack.push(self.stack[comp_idx]);
            self.stack.push(a);
            self.stack.push(b);
            self.call(ArgCount::Fixed(2), RetCount::Fixed(1))?;
            return Ok(self.pop_val().truthy());
        }

        match (a, b) {
            (Val::Num(a), Val::Num(b)) => Ok(a < b),
            (Val::Str(a), Val::Str(b)) => Ok(self.heap.get_string(a) < self.heap.get_string(b)),
            (a, b) => Err(self.error(ErrorKind::TypeError(TypeError::Comparison(
                a.typ(&self.heap),
                b.typ(&self.heap),
            )))),
        }
    }

    /// Converts the value at the given index to a string, checking for __tostring metamethod.
    /// If the value is a table with a __tostring metamethod, calls it and returns the result.
    #[hotpath::measure]
    pub fn to_string_with_meta(&mut self, idx: isize) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.bytes_with_tostring_meta(idx)?).into_owned())
    }

    /// Converts a value with `tostring` semantics while preserving arbitrary
    /// bytes returned by a `__tostring` metamethod.
    pub(crate) fn bytes_with_tostring_meta(&mut self, idx: isize) -> Result<Vec<u8>> {
        let i = self.convert_idx(idx)?;
        let val = self.stack[i];
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
                self.stack.push(tostring_handler);
                self.stack.push(val);
                self.call(ArgCount::Fixed(1), RetCount::Fixed(1))?;
                let result = self.pop_val();
                if matches!(
                    result.typ(&self.heap),
                    super::LuaType::String | super::LuaType::Number
                ) {
                    return Ok(result.to_bytes_with_heap(&self.heap));
                }
                return Err(self.error(ErrorKind::RuntimeError(
                    "'__tostring' must return a string".to_string(),
                )));
            }
        }

        self.bytes_with_default_string_coercion(idx)
    }

    pub(crate) fn bytes_with_default_string_coercion(&mut self, idx: isize) -> Result<Vec<u8>> {
        let i = self.convert_idx(idx)?;
        let val = self.stack[i];
        if matches!(val, Val::Obj(_)) {
            let id = self.format_pointer_id(idx)?;
            return Ok(format!("{}: 0x{id:x}", val.typ(&self.heap).as_str()).into_bytes());
        }
        Ok(val.to_bytes_with_heap(&self.heap))
    }

    /// Returns a deterministic, state-local identity for pointer-like values.
    pub(crate) fn format_pointer_id(&mut self, idx: isize) -> Result<u64> {
        let val = self.at_index(idx)?;
        if let Some((_, id)) = self
            .format_pointer_ids
            .iter()
            .find(|(candidate, _)| *candidate == val)
        {
            return Ok(*id);
        }
        let id = self.next_format_pointer_id;
        self.next_format_pointer_id = self.next_format_pointer_id.wrapping_add(1);
        self.format_pointer_ids.push((val, id));
        Ok(id)
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
