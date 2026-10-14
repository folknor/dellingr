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
    pub fn new_table(&mut self) -> Result<()> {
        self.check_stack_space(1)?;
        let val = self.alloc_table();
        self.push_unchecked(val);
        Ok(())
    }

    #[hotpath::measure]
    pub(crate) fn new_table_with_capacity(&mut self, capacity: usize) -> Result<()> {
        self.check_stack_space(1)?;
        let val = self.alloc_table_with_capacity(capacity);
        self.push_unchecked(val);
        Ok(())
    }

    #[hotpath::measure]
    pub(super) fn new_table_with_template(
        &mut self,
        key_ids: &[u16],
        literals: &[Val],
    ) -> Result<()> {
        self.check_stack_space(1)?;
        self.maybe_collect();
        let obj = self.heap.alloc_table_with_template(key_ids, literals);
        self.push_unchecked(Val::Obj(obj));
        Ok(())
    }

    pub(crate) fn set_table_str_key_value(
        &mut self,
        table_idx: isize,
        name: &str,
        val: Val,
    ) -> Result<()> {
        self.with_rooted_value(val, |state| {
            let key = state.alloc_string(name)?;
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
            .map_err(Self::registration_error)?;
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
    ///
    /// Validation and dispatch preflight failures leave the stack unchanged.
    /// An error from calling an `__index` function is not covered: by then
    /// the key has been consumed, and a script error kills the callback anyway.
    #[hotpath::measure]
    pub fn get_table(&mut self, i: isize) -> Result<()> {
        let idx = self.table_operand_below_top(i, 1)?;
        let top = self.stack.len();
        let key = self.pop_val();
        let mut local_cost = 0;
        let mut handler_called = false;
        let result =
            self.get_table_with_key_tracking_call(idx, key, &mut local_cost, &mut handler_called);
        if result.is_err() && !handler_called {
            // No user code ran. Dispatch roots the key through its allocations
            // and can only have added temporary slots above the entry prefix.
            self.stack.truncate(top - 1);
            self.push_unchecked(key); // Replaces the key popped above.
        }
        result
    }

    /// Resolves `i` to a table that sits below the top `operands` values, and
    /// type-checks it, without touching the stack. Host methods that consume
    /// operands call this first so that every rejection they can foresee
    /// leaves the stack unchanged (peek, validate, then pop on commit).
    fn table_operand_below_top(&self, i: isize, operands: usize) -> Result<usize> {
        let idx = self.convert_idx(i)?;
        // The operands must sit ABOVE the table, so that popping them leaves
        // the table itself in place. A host calling with too few visible
        // values is a mistake, not a VM bug, so it must produce an error
        // rather than reach the panicking `pop_val`. The error names the
        // table index, since it is the one that leaves no room above it.
        if self.stack.len() < idx + 1 + operands {
            return Err(self.error(ErrorKind::InvalidStackIndex { index: i }));
        }
        if self.stack[idx]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .is_none()
        {
            return Err(self.type_error(TypeError::TableIndex(self.stack[idx].typ(&self.heap))));
        }
        Ok(idx)
    }

    /// Gets `t[k]` without invoking metamethods.
    /// `t` is at the given index, `k` is at the top of the stack.
    /// Pops the key and pushes the result. On error the stack is unchanged.
    #[hotpath::measure]
    pub fn get_table_raw(&mut self, i: isize) -> Result<()> {
        let idx = self.table_operand_below_top(i, 1)?;
        let key = self.stack[self.stack.len() - 1];
        let val = self.stack[idx]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .expect("table_operand_below_top validated the table")
            .get(&key);
        // Replaces the key in place: net-neutral, so no preflight is needed.
        let top = self.stack.len() - 1;
        self.stack[top] = val;
        Ok(())
    }

    /// Does the equivalent of `t[k] = v`, where `t` is the value at the given
    /// valid index, `k` is the value at the top of the stack minus 1, and `v`
    /// is the value at the top of the stack.
    ///
    /// This function pops both the key and the value from the stack. Matches
    /// the reference Lua C API's `lua_rawset`. A bad index, a non-table
    /// target, or a nil/NaN key is rejected before either operand is popped.
    #[hotpath::measure]
    pub fn set_table_raw(&mut self, i: isize) -> Result<()> {
        let idx = self.table_operand_below_top(i, 2)?;
        let top = self.stack.len();
        let key = self.stack[top - 2];
        let val = self.stack[top - 1];
        match key {
            Val::Nil => return Err(self.type_error(TypeError::TableKeyNil)),
            Val::Num(n) if n.is_nan() => return Err(self.type_error(TypeError::TableKeyNan)),
            _ => {}
        }
        self.stack[idx]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table(ptr))
            .expect("table_operand_below_top validated the table")
            .insert(key, val)?;
        self.stack.truncate(top - 2);
        Ok(())
    }

    /// Library assignment with the same `__newindex` dispatch as bytecode.
    /// Keep the key and value on the stack as GC roots throughout dispatch;
    /// consume them only after a successful write or handler call.
    pub(crate) fn set_table(&mut self, i: isize) -> Result<()> {
        let idx = self.table_operand_below_top(i, 2)?;
        let top = self.stack.len();
        let key = self.stack[top - 2];
        let val = self.stack[top - 1];
        let mut local_cost = 0;
        self.set_table_with_key(idx, key, val, &mut local_cost)?;
        self.stack.truncate(top - 2);
        Ok(())
    }

    /// Returns the next key-value pair from a table, for use with `pairs`.
    /// Takes the table index and pops the key from the stack.
    /// Pushes the next key and value onto the stack (or just nil if done).
    /// Every failure (bad index, non-table, invalid key, stack overflow) is
    /// detected before the key is popped, so a host loop can retry or resume.
    #[hotpath::measure]
    pub fn table_next(&mut self, table_idx: isize) -> Result<bool> {
        let idx = self.table_operand_below_top(table_idx, 1)?;
        let top = self.stack.len() - 1;
        let key = self.stack[top];
        let next = self.stack[idx]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .expect("table_operand_below_top validated the table")
            .next(&key);
        match next {
            TableNext::Pair(next_key, next_val) => {
                // The key slot is reused for `next_key`, so only the value
                // needs a fresh slot. Checked before anything is written.
                self.check_stack_space(1)?;
                self.stack[top] = next_key;
                self.push_unchecked(next_val);
                Ok(true)
            }
            TableNext::End => {
                self.stack[top] = Val::Nil;
                Ok(false)
            }
            TableNext::InvalidKey => {
                Err(self.error(ErrorKind::RuntimeError("invalid key to 'next'".into())))
            }
        }
    }

    /// Returns the length (border) of the table at a stack index, as the `#`
    /// operator would without metamethods; for a string, its byte length.
    /// Matches the reference C API's `lua_rawlen`, except that a value with
    /// no length is an error rather than 0, so a host bug cannot read as an
    /// empty table. A bad index is `InvalidStackIndex`.
    pub fn raw_len(&self, idx: isize) -> Result<usize> {
        let i = self.convert_idx(idx)?;
        match self.stack[i] {
            Val::Str(s) => Ok(self.heap.get_string(s).len()),
            v => v
                .as_object_ptr()
                .and_then(|ptr| self.heap.as_table_ref(ptr))
                .map(super::table::Table::array_len)
                .ok_or_else(|| self.type_error(TypeError::Length(v.typ(&self.heap)))),
        }
    }

    /// Returns the array length of the table at the given index, or 0 for a
    /// bad index or a non-table. Crate-internal: stdlib callers have already
    /// type-checked the argument, so the 0 fallback is unreachable for them.
    /// Hosts use [`State::raw_len`], which reports both mistakes.
    #[hotpath::measure]
    pub(crate) fn table_len(&self, idx: isize) -> usize {
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
    /// For tables, returns the table's metatable. For strings, returns the
    /// metatable all strings share, as reference Lua does (`__index` = the
    /// string library table `open_libs` built; nil in a `State::empty` that
    /// never opened the libraries). It is a real table a script may modify;
    /// `index_string` in `eval_index.rs` resolves string indexing through it.
    /// It cannot be replaced: `set_metatable_of` accepts only tables. Every
    /// other type has no metatable and returns nil.
    #[hotpath::measure]
    pub fn get_metatable_of(&mut self, idx: isize) -> Result<()> {
        let i = self.convert_idx(idx)?;

        let val = self.stack[i];
        let metatable = if val.as_string_ptr().is_some() {
            self.string_metatable
        } else {
            val.as_object_ptr()
                .and_then(|ptr| self.heap.as_table_ref(ptr))
                .and_then(super::table::Table::get_metatable)
        };

        match metatable {
            Some(mt) => self.push_val(Val::Obj(mt))?,
            None => self.push_nil()?,
        }
        Ok(())
    }

    /// Sets the metatable of the table at the given index.
    /// The metatable should be at the top of the stack (or nil to remove).
    /// Pops the metatable from the stack. A bad index, a non-table target or
    /// a metatable that is neither a table nor nil is rejected before the
    /// pop, leaving the stack unchanged.
    #[hotpath::measure]
    pub fn set_metatable_of(&mut self, table_idx: isize) -> Result<()> {
        let idx = self.table_operand_below_top(table_idx, 1)?;
        let mt = match self.stack[self.stack.len() - 1] {
            Val::Nil => None,
            Val::Obj(ptr) if self.heap.as_table_ref(ptr).is_some() => Some(ptr),
            other => return Err(self.type_error(TypeError::TableIndex(other.typ(&self.heap)))),
        };
        self.stack[idx]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table(ptr))
            .expect("table_operand_below_top validated the table")
            .set_metatable(mt);
        self.pop_val();
        Ok(())
    }

    /// Inserts a value into a table at a position, shifting elements.
    /// Stack: [t, value] -> [t]
    /// `table_idx` resolves against the entry stack, so a negative index counts
    /// from the top value that will be popped. The value is popped from the
    /// stack. `pos` must be in `1..=raw_len(table_idx) + 1`; a bad index, a
    /// non-table or an invalid position leaves the stack and table unchanged.
    /// Charges no cost; the Lua `table.insert` wrapper charges a flat 1.
    #[hotpath::measure]
    pub fn table_insert_at(&mut self, table_idx: isize, pos: usize) -> Result<()> {
        // Resolve against the entry stack, including the value to be popped.
        // This keeps negative indices relative to the caller's visible top and
        // validates the table before mutating the stack.
        let idx = self.table_operand_below_top(table_idx, 1)?;
        if !(1..=self.table_len(table_idx) + 1).contains(&pos) {
            return Err(self.error(ErrorKind::RuntimeError("position out of bounds".into())));
        }
        let value = self.pop_val();
        self.stack[idx]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table(ptr))
            .expect("table_operand_below_top validated the table")
            .array_insert(pos, value);
        Ok(())
    }

    /// Removes a value from a table at a position, shifting elements down,
    /// and pushes the removed value onto the stack.
    ///
    /// `pos` must be in `1..=raw_len(table_idx) + 1`, or 0 when the table's
    /// length is 0 - the range `table.remove` accepts. Positions 0 and
    /// `len + 1` shift nothing: they return `t[pos]` and clear it, as
    /// reference `table.remove` does. Any other position is an error that
    /// leaves the stack and table unchanged; without this check a stray
    /// position would silently clear an arbitrary key. Charges no cost.
    #[hotpath::measure]
    pub fn table_remove_at(&mut self, table_idx: isize, pos: usize) -> Result<()> {
        let idx = self.table_operand_below_top(table_idx, 0)?;
        let len = self.table_len(table_idx);
        let valid = (1..=len + 1).contains(&pos) || (len == 0 && pos == 0);
        if !valid {
            return Err(self.error(ErrorKind::RuntimeError("position out of bounds".into())));
        }

        // Reserve the result slot *before* mutating the table: failing after
        // `array_remove` would discard the value with no way to return it.
        self.check_stack_space(1)?;
        let removed = self.stack[idx]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table(ptr))
            .expect("table_operand_below_top validated the table")
            .array_remove(pos);
        self.push_unchecked(removed);
        Ok(())
    }

    /// Sorts the sequence `t[1..=#t]` of the table at `table_idx` in place.
    /// If `has_comp` is true, the function at frame index 2 is the comparator.
    /// Charges `max(n, 1)` itself, before the comparator runs or the table is
    /// mutated, and returns `n`; callers must not charge again.
    ///
    /// Crate-internal (VM-007): the comparator slot is `table.sort`'s argument
    /// layout, a stdlib calling convention a host has no reason to mirror.
    /// Hosts sort by calling `table.sort` through the stack.
    #[hotpath::measure]
    pub(crate) fn table_sort(&mut self, table_idx: isize, has_comp: bool) -> Result<usize> {
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
        // only then failing the charge. `arr` is a detached copy here, so
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
            // Comparator plus two operands: net-positive by three slots until
            // the call consumes them, so preflight before any mutation.
            self.check_stack_space(3)?;
            self.push_unchecked(self.stack[comp_idx]);
            self.push_unchecked(a);
            self.push_unchecked(b);
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

    /// Converts the value at the given index to a UTF-8 string, checking for
    /// a `__tostring` metamethod. Invalid UTF-8 bytes (from a Lua string or a
    /// `__tostring` result) are converted lossily; use
    /// [`State::bytes_with_tostring_meta`] for the exact bytes.
    #[hotpath::measure]
    pub fn to_string_with_meta(&mut self, idx: isize) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.bytes_with_tostring_meta(idx)?).into_owned())
    }

    /// Converts a value with `tostring` semantics while preserving arbitrary
    /// bytes returned by a `__tostring` metamethod. This is what the
    /// `tostring` builtin returns.
    pub fn bytes_with_tostring_meta(&mut self, idx: isize) -> Result<Vec<u8>> {
        let i = self.convert_idx(idx)?;
        let val = self.stack[i];
        let metatable_ptr = val
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .and_then(super::table::Table::get_metatable);

        if let Some(mt_ptr) = metatable_ptr {
            // Keep this dispatch-time intern at its existing allocation point;
            // the receiver remains rooted on the stack through collection.
            // Pre-interning moves that allocation and can change reuse order.
            let tostring_key = self.alloc_string("__tostring")?;
            let tostring_handler = self
                .heap
                .as_table_ref(mt_ptr)
                .map_or(Val::Nil, |mt| mt.get(&tostring_key));

            if !matches!(tostring_handler, Val::Nil) {
                // Handler plus receiver: net-positive by two slots until the
                // call consumes them.
                self.check_stack_space(2)?;
                self.push_unchecked(tostring_handler);
                self.push_unchecked(val);
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
        // Rust functions get the same state-local identity as closures, so
        // distinct builtins (unequal under `==`, which compares addresses)
        // render distinctly. The id map is keyed by the function value and is
        // saved state: a registered RustFn reachable at save time encodes as
        // a registry token, so its id survives save and load.
        if matches!(val, Val::Obj(_) | Val::RustFn(_)) {
            let id = self.format_pointer_id(idx)?;
            return Ok(format!("{}: 0x{id:x}", val.typ(&self.heap).as_str()).into_bytes());
        }
        Ok(val.to_bytes_with_heap(&self.heap))
    }

    /// Returns a deterministic, state-local identity for pointer-like values.
    pub(crate) fn format_pointer_id(&mut self, idx: isize) -> Result<u64> {
        let val = self.at_index(idx)?;
        // Callers request identities only for strings, objects and functions;
        // numbers (including NaN) never enter this map.
        debug_assert!(matches!(val, Val::Str(_) | Val::Obj(_) | Val::RustFn(_)));
        if let Some(id) = self.format_pointer_ids.get(&val) {
            return Ok(*id);
        }
        let id = self.next_format_pointer_id;
        // A snapshot can place the counter at the end of its range. Refuse a
        // fresh identity before mutating either field: wrapping would reissue
        // old ids and violate the snapshot verifier's id < next invariant.
        let next = id.checked_add(1).ok_or_else(|| {
            self.error(ErrorKind::RuntimeError(
                "format pointer identity space exhausted".to_string(),
            ))
        })?;
        self.next_format_pointer_id = next;
        self.format_pointer_ids.insert(val, id);
        Ok(id)
    }

    /// Allocates a new table on the heap.
    #[hotpath::measure]
    pub(super) fn alloc_table(&mut self) -> Val {
        self.maybe_collect();
        let obj = self.heap.alloc_table();
        Val::Obj(obj)
    }

    pub(super) fn alloc_table_with_capacity(&mut self, capacity: usize) -> Val {
        self.maybe_collect();
        let obj = self.heap.alloc_table_with_capacity(capacity);
        Val::Obj(obj)
    }
}
