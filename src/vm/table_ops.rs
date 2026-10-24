//! Table operations for the Lua VM.
//!
//! This module contains methods for creating and manipulating Lua tables,
//! including metamethod-aware operations.

use super::lua_val::{PointerIdKey, RustFunc, Val};
use super::table::{TableError, TableNext};
use super::{Result, State, TypeError};
use crate::error::{Error, ErrorKind, ErrorValue, InternalFault, LibraryError, LibraryFunction};
use crate::instr::{ArgCount, RetCount};

/// How a value renders under `tostring`; see `State::tostring_rendering`.
pub(crate) enum TostringRendering {
    /// A string with no `__tostring` handler: the value is its own rendering,
    /// so a caller can return the original stack value without re-interning.
    Itself,
    /// The rendered bytes.
    Bytes(Vec<u8>),
}

trait TableSortAccess {
    fn get(&mut self, state: &mut State, index: usize) -> Result<Val>;
    fn set(
        &mut self,
        state: &mut State,
        index: usize,
        value: Val,
    ) -> std::result::Result<(), TableError>;
}

fn reserve_sort_values(values: &mut Vec<Val>, additional: usize) -> Result<()> {
    values
        .try_reserve_exact(additional)
        .map_err(|_| Error::without_location(ErrorKind::TableGrowthFailed))?;
    Ok(())
}

struct VecSortAccess<'a> {
    values: &'a mut [Val],
}

impl TableSortAccess for VecSortAccess<'_> {
    fn get(&mut self, _state: &mut State, index: usize) -> Result<Val> {
        Ok(self.values[index])
    }

    fn set(
        &mut self,
        _state: &mut State,
        index: usize,
        value: Val,
    ) -> std::result::Result<(), TableError> {
        self.values[index] = value;
        Ok(())
    }
}

struct ScratchTableSortAccess {
    table_idx: usize,
}

impl TableSortAccess for ScratchTableSortAccess {
    fn get(&mut self, state: &mut State, index: usize) -> Result<Val> {
        let ptr = state.stack[self.table_idx]
            .as_object_ptr()
            .expect("scratch sort stack slot holds a table");
        let table = state
            .heap
            .as_table_ref(ptr)
            .expect("scratch sort object is a table");
        Ok(table.get(&Val::Num((index + 1) as f64)))
    }

    fn set(
        &mut self,
        state: &mut State,
        index: usize,
        value: Val,
    ) -> std::result::Result<(), TableError> {
        let ptr = state.stack[self.table_idx]
            .as_object_ptr()
            .expect("scratch sort stack slot holds a table");
        let table = state
            .heap
            .as_table(ptr)
            .expect("scratch sort object is a table");
        table.insert(Val::Num((index + 1) as f64), value)
    }
}

fn table_sort_heap<A: TableSortAccess>(
    state: &mut State,
    access: &mut A,
    n: usize,
    comp_idx: Option<usize>,
) -> Result<()> {
    for root in (0..n / 2).rev() {
        crate::native_call!(
            "table_sort_heap->table_sort_sift_down:heapify",
            table_sort_sift_down(state, access, root, n, comp_idx)
        )?;
    }
    for end in (1..n).rev() {
        table_sort_swap(state, access, 0, end)?;
        crate::native_call!(
            "table_sort_heap->table_sort_sift_down:sortdown",
            table_sort_sift_down(state, access, 0, end, comp_idx)
        )?;
    }
    for index in 0..n / 2 {
        table_sort_swap(state, access, index, n - 1 - index)?;
    }
    Ok(())
}

fn table_sort_sift_down<A: TableSortAccess>(
    state: &mut State,
    access: &mut A,
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
        let left_value = access.get(state, left)?;
        let smallest_value = access.get(state, smallest)?;
        if crate::native_call!(
            "table_sort_sift_down->table_sort_less:left",
            state.table_sort_less(left_value, smallest_value, comp_idx)
        )? {
            smallest = left;
        }
        let right = left + 1;
        if right < end {
            let right_value = access.get(state, right)?;
            let smallest_value = access.get(state, smallest)?;
            if crate::native_call!(
                "table_sort_sift_down->table_sort_less:right",
                state.table_sort_less(right_value, smallest_value, comp_idx)
            )? {
                smallest = right;
            }
        }
        if smallest == root {
            return Ok(());
        }
        table_sort_swap(state, access, root, smallest)?;
        root = smallest;
    }
}

fn table_sort_swap<A: TableSortAccess>(
    state: &mut State,
    access: &mut A,
    a: usize,
    b: usize,
) -> Result<()> {
    if a == b {
        return Ok(());
    }
    let a_value = access.get(state, a)?;
    let b_value = access.get(state, b)?;
    access.set(state, a, b_value)?;
    access.set(state, b, a_value)?;
    Ok(())
}

impl State {
    /// Creates a new empty table and pushes it onto the stack.
    pub fn new_table(&mut self) -> Result<()> {
        self.check_poisoned()?;
        self.push_new_table()
    }

    /// [`State::new_table`] without the poison check, for the dispatch loop.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(super) fn push_new_table(&mut self) -> Result<()> {
        self.check_stack_space(1)?;
        let val = self.alloc_table();
        self.push_unchecked(val);
        Ok(())
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(crate) fn new_table_with_capacity(&mut self, capacity: usize) -> Result<()> {
        self.check_stack_space(1)?;
        let val = self.alloc_table_with_capacity(capacity);
        self.push_unchecked(val);
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
    /// A string receiver resolves through the shared string metatable exactly
    /// as the `t[k]` opcode does (`State::resolve_index`), so `("abc")["upper"]` gives
    /// the host what it gives a script.
    ///
    /// Validation and dispatch preflight failures leave the stack unchanged.
    /// An error from calling an `__index` function consumes the key and clears
    /// handler temporaries, preserving every earlier stack slot. Writes made
    /// by the handler survive the error.
    ///
    /// An invalid index or a missing key is host misuse and does not fire
    /// [`HostCallbacks::on_error`](crate::HostCallbacks::on_error); a
    /// receiver that cannot be indexed, and every failure of the lookup
    /// itself, notify as `call`'s errors do.
    ///
    /// Guarded like [`State::call`]: refused on a poisoned State, and a
    /// panic out of a metamethod poisons it.
    pub fn get_table(&mut self, i: isize) -> Result<()> {
        self.guarded(|state| {
            crate::native_call!(
                "get_table->get_table_unguarded",
                state.get_table_unguarded(i)
            )
        })
    }

    /// [`State::get_table`] without the poison boundary, for the standard
    /// library and other VM-internal callers.
    // This frame survives __index re-entry; instrumentation would add to
    // every level of the bounded native stack. Measure non-reentrant callees.
    pub(crate) fn get_table_unguarded(&mut self, i: isize) -> Result<()> {
        let receiver_idx = self.convert_idx(i)?;
        let is_string = self.stack.len() >= receiver_idx + 2
            && self.stack[receiver_idx].as_string_ptr().is_some();
        let idx = if is_string {
            receiver_idx
        } else {
            self.table_operand_below_top(i, 1)
                .map_err(|error| self.report_index_receiver_error(error))?
        };
        let top = self.stack.len();
        let key = self.pop_val();
        let receiver = self.stack[idx];
        let mut local_cost = 0;
        let mut handler_called = false;
        let result = match self.resolve_index(
            receiver,
            super::metamethod::IndexStart::Receiver,
            key,
            None,
            &mut local_cost,
        ) {
            Ok(super::metamethod::IndexOutcome::Value(val)) => Ok(val),
            Ok(super::metamethod::IndexOutcome::Call(units)) => {
                let called = crate::native_call!(
                    "get_table_unguarded->call_unguarded",
                    self.call_unguarded(
                        (ArgCount::Fixed(2), &mut handler_called),
                        RetCount::Fixed(1),
                    )
                );
                self.in_flight.metamethod_depth -= units;
                called.map(|()| self.pop_val())
            }
            Err(error) => Err(error),
        };
        match result {
            // Replaces the key popped above: net zero.
            Ok(val) => {
                self.push_unchecked(val);
                Ok(())
            }
            Err(error) => {
                // Clear the call window. Before handler entry the dispatch
                // contract also preserves the popped key.
                self.stack.truncate(top - 1);
                if !handler_called {
                    self.push_unchecked(key);
                }
                Err(self.report_escaping(error))
            }
        }
    }

    /// Classify `get_table`'s operand rejection. Operand misuse (a bad index,
    /// no key above the receiver) is returned unreported, as `call`'s
    /// preflight is (see the on_error doc in host.rs). A receiver that cannot
    /// be indexed is a runtime type error, as indexing it in a script is, so
    /// it is reported like the failures after dispatch. Either way the stack
    /// is unchanged.
    #[cold]
    #[inline(never)]
    fn report_index_receiver_error(&mut self, error: Error) -> Error {
        if matches!(error.kind(), ErrorKind::TypeError(_)) {
            self.report_escaping(error)
        } else {
            error
        }
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
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn get_table_raw(&mut self, i: isize) -> Result<()> {
        self.check_poisoned()?;
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
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn set_table_raw(&mut self, i: isize) -> Result<()> {
        self.check_poisoned()?;
        let idx = self.table_operand_below_top(i, 2)?;
        let top = self.stack.len();
        let key = self.stack[top - 2];
        let val = self.stack[top - 1];
        key.table_key().map_err(|error| match error {
            super::table::InvalidTableKey::Nil => self.type_error(TypeError::TableKeyNil),
            super::table::InvalidTableKey::Nan => self.type_error(TypeError::TableKeyNan),
        })?;
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
    /// consume them only after a successful write or handler call. On error
    /// the key, value and handler temporaries stay for the enclosing call
    /// boundary or dispatch-loop unwind.
    pub(crate) fn set_table(&mut self, i: isize) -> Result<()> {
        let idx = self.table_operand_below_top(i, 2)?;
        let top = self.stack.len();
        let key = self.stack[top - 2];
        let val = self.stack[top - 1];
        let mut local_cost = 0;
        let ptr = self.stack[idx]
            .as_object_ptr()
            .expect("table_operand_below_top validated the table");
        let tbl = self
            .heap
            .as_table(ptr)
            .expect("table_operand_below_top validated the table");
        let pending = if matches!(tbl.get(&key), Val::Nil) {
            self.store_new_key(ptr, key, val, &mut local_cost)?
        } else {
            tbl.insert(key, val)?;
            None
        };
        if let Some(units) = pending {
            let called = crate::native_call!(
                "set_table->call_unguarded",
                self.call_unguarded(ArgCount::Fixed(3), RetCount::Fixed(0))
            );
            self.in_flight.metamethod_depth -= units;
            called?;
        }
        self.stack.truncate(top - 2);
        Ok(())
    }

    /// Returns the next key-value pair from a table, for use with `pairs`.
    /// Takes the table index and pops the key from the stack.
    /// Pushes the next key and value onto the stack (or just nil if done).
    /// Every failure (bad index, non-table, invalid key, stack overflow) is
    /// detected before the key is popped, so a host loop can retry or resume.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn table_next(&mut self, table_idx: isize) -> Result<bool> {
        self.check_poisoned()?;
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
            TableNext::InvalidKey => Err(self.library_error(LibraryError::InvalidNextKey)),
        }
    }

    /// Returns the length (border) of the table at a stack index, as the `#`
    /// operator would without metamethods; for a string, its byte length.
    /// Matches the reference C API's `lua_rawlen`, except that a value with
    /// no length is an error rather than 0, so a host bug cannot read as an
    /// empty table. A bad index is `InvalidStackIndex`.
    pub fn raw_len(&self, idx: isize) -> Result<usize> {
        self.check_poisoned()?;
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
    /// non-table value. Returns [`ErrorKind::InvalidStackIndex`] for an
    /// invalid index. Standard library callers validate the argument type.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(crate) fn table_len(&self, idx: isize) -> Result<usize> {
        let i = self.convert_idx(idx)?;
        Ok(self.stack[i]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .map_or(0, super::table::Table::array_len))
    }

    fn table_array_len_at(&self, idx: usize) -> usize {
        self.stack[idx]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .expect("table_operand_below_top validated the table")
            .array_len()
    }

    /// Gets the metatable of the value at the given index.
    /// For tables, returns the table's metatable. For strings, returns the
    /// metatable all strings share, as reference Lua does (`__index` = the
    /// string library table `open_libs` built; nil in a `State::raw` that
    /// never opened the libraries). It is a real table a script may modify;
    /// `State::resolve_index` resolves string indexing through it.
    /// It cannot be replaced: `set_metatable_of` accepts only tables. Every
    /// other type has no metatable and returns nil.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn get_metatable_of(&mut self, idx: isize) -> Result<()> {
        self.check_poisoned()?;
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
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn set_metatable_of(&mut self, table_idx: isize) -> Result<()> {
        self.check_poisoned()?;
        let idx = self.table_operand_below_top(table_idx, 1)?;
        let mt = match self.stack[self.stack.len() - 1] {
            Val::Nil => None,
            Val::Obj(ptr) if self.heap.as_table_ref(ptr).is_some() => Some(ptr),
            _ => return Err(self.type_error(TypeError::InvalidMetatable)),
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
    /// Stack: `[t, value] -> [t]`
    /// `table_idx` resolves against the entry stack, so a negative index counts
    /// from the top value that will be popped. The value is popped from the
    /// stack. `pos` must be in `1..=raw_len(table_idx) + 1`; a bad index, a
    /// non-table or an invalid position leaves the stack and table unchanged.
    /// Growth failure also leaves the value on the stack.
    /// Charges no cost; the Lua `table.insert` wrapper charges a flat 1.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn table_insert_at(&mut self, table_idx: isize, pos: usize) -> Result<()> {
        self.check_poisoned()?;
        // Resolve against the entry stack, including the value to be popped.
        // This keeps negative indices relative to the caller's visible top and
        // validates the table before mutating the stack.
        let idx = self.table_operand_below_top(table_idx, 1)?;
        if !(1..=self.table_array_len_at(idx) + 1).contains(&pos) {
            return Err(self.library_error(LibraryError::PositionOutOfBounds));
        }
        let value = self.stack[self.stack.len() - 1];
        self.stack[idx]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table(ptr))
            .expect("table_operand_below_top validated the table")
            .array_insert(pos, value)?;
        self.pop_val();
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
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn table_remove_at(&mut self, table_idx: isize, pos: usize) -> Result<()> {
        self.check_poisoned()?;
        let idx = self.table_operand_below_top(table_idx, 0)?;
        let len = self.table_array_len_at(idx);
        let valid = (1..=len + 1).contains(&pos) || (len == 0 && pos == 0);
        if !valid {
            return Err(self.library_error(LibraryError::PositionOutOfBounds));
        }

        // Reserve the result slot *before* mutating the table: failing after
        // `array_remove` would discard the value with no way to return it.
        self.check_stack_space(1)?;
        let removed = self.stack[idx]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table(ptr))
            .expect("table_operand_below_top validated the table")
            .array_remove(pos)?;
        self.push_unchecked(removed);
        Ok(())
    }

    /// Sorts the sequence `t[1..=#t]` of the table at `table_idx` in place.
    /// If `has_comp` is true, the function at frame index 2 is the comparator.
    /// Charges `max(n, 1)` itself, before the comparator runs or the table is
    /// mutated, and returns `n`; callers must not charge again.
    ///
    /// The comparator slot follows `table.sort`'s argument layout, a stdlib
    /// calling convention a host has no reason to mirror.
    /// Hosts sort by calling `table.sort` through the stack.
    pub(crate) fn table_sort(&mut self, table_idx: isize, has_comp: bool) -> Result<usize> {
        let idx = self.convert_idx(table_idx)?;

        // Check the raw array length before cloning it, so the same size rule
        // guards both raw and metamethod-backed sorts.
        let obj_ptr = self.stack[idx].as_object_ptr();
        let typ = self.stack[idx].typ(&self.heap);

        let table_ptr = match obj_ptr.and_then(|ptr| self.heap.as_table_ref(ptr).map(|_| ptr)) {
            Some(ptr) => ptr,
            None => return Err(self.type_error(TypeError::TableIndex(typ))),
        };
        let array_len = self
            .heap
            .as_table_ref(table_ptr)
            .expect("validated object pointer is a table")
            .array_len();
        // The array part is bounded far below i64::MAX; saturate rather than panic.
        let len = i64::try_from(array_len).map_or(i64::MAX, i64::from);
        let n = self.checked_table_sort_len(len)?;
        let mut arr = self
            .heap
            .as_table_ref(table_ptr)
            .expect("validated object pointer is a table")
            .get_array()?;

        // Charge cost BEFORE running the comparator or mutating the table, so an
        // exhausted budget blocks the sort rather than letting it complete and
        // only then failing the charge. `arr` is a detached copy here, so
        // nothing has been mutated yet. Empty sorts still cost 1.
        self.consume_cost(n.max(1) as u64)?;

        if arr.is_empty() {
            return Ok(0);
        }

        let comp_idx = has_comp.then(|| self.convert_idx(2)).transpose()?;

        // Heap sort remains bounded and deterministic even when a Lua
        // comparator is inconsistent. Unlike reference quicksort, it does not
        // diagnose that incidental invalid-order case.
        match comp_idx {
            // A comparator can re-enter Lua and force a collection, and `arr` is
            // a detached `Vec<Val>` that GC cannot see, so the whole array has
            // to stay rooted for the entire sort - not just the pair being
            // compared. `src/vm/tests.rs` covers a comparator that clears the
            // source table before collecting.
            Some(_) => {
                // Both copies can be as large as the sorted array. Make their
                // growth fallible before copying or entering the comparator.
                let mut roots = Vec::new();
                reserve_sort_values(&mut roots, arr.len())?;
                roots.extend_from_slice(&arr);
                reserve_sort_values(&mut self.in_flight.rooted_values, roots.len())?;
                self.with_rooted_values(&roots, |state| {
                    crate::native_call!(
                        "table_sort->table_sort_values:comparator",
                        state.table_sort_values(&mut arr, comp_idx)
                    )
                })?;
            }
            // The default comparison only reads numbers and already-interned
            // string bytes: it cannot call Lua or trigger GC (its failure
            // allocates only the Rust error, not VM objects), and the
            // source table holds every element until writeback. Rooting here
            // would just copy the array twice more for nothing.
            None => crate::native_call!(
                "table_sort->table_sort_values:default",
                self.table_sort_values(&mut arr, comp_idx)
            )?,
        }

        // Put sorted array back - need to look up the table again since we may have
        // mutated self during comparator calls
        let obj_ptr = self.stack[idx].as_object_ptr();
        match obj_ptr.and_then(|ptr| self.heap.as_table(ptr)) {
            Some(t) => {
                t.set_array(arr)?;
                Ok(n)
            }
            None => Err(self.type_error(TypeError::TableIndex(typ))),
        }
    }

    pub(crate) fn table_sort_scratch(
        &mut self,
        table_idx: isize,
        n: usize,
        has_comp: bool,
    ) -> Result<()> {
        let table_idx = self.convert_idx(table_idx)?;
        let comp_idx = has_comp.then(|| self.convert_idx(2)).transpose()?;
        crate::native_call!(
            "table_sort_scratch->table_sort_heap",
            table_sort_heap(self, &mut ScratchTableSortAccess { table_idx }, n, comp_idx)
        )
    }

    fn table_sort_values(&mut self, values: &mut [Val], comp_idx: Option<usize>) -> Result<()> {
        let n = values.len();
        let mut access = VecSortAccess { values };
        crate::native_call!(
            "table_sort_values->table_sort_heap",
            table_sort_heap(self, &mut access, n, comp_idx)
        )
    }

    pub(crate) fn checked_table_sort_len(&self, len: i64) -> Result<usize> {
        if len >= crate::limits::TABLE_SORT_MAX_LEN {
            Err(self.table_sort_too_big_error())
        } else if len < 0 {
            Ok(0)
        } else {
            usize::try_from(len).map_err(|_| self.table_sort_too_big_error())
        }
    }

    fn table_sort_too_big_error(&self) -> crate::error::Error {
        self.library_error(LibraryError::ArrayTooBig)
    }

    fn table_sort_less(&mut self, a: Val, b: Val, comp_idx: Option<usize>) -> Result<bool> {
        if let Some(comp_idx) = comp_idx {
            // Comparator plus two operands: net-positive by three slots until
            // the call consumes them, so preflight before any mutation.
            self.check_stack_space(3)?;
            self.push_unchecked(self.stack[comp_idx]);
            self.push_unchecked(a);
            self.push_unchecked(b);
            crate::native_call!(
                "table_sort_less->call_unguarded",
                self.call_unguarded(ArgCount::Fixed(2), RetCount::Fixed(1))
            )?;
            return Ok(self.pop_val().truthy());
        }

        match (a, b) {
            (Val::Num(a), Val::Num(b)) => Ok(a < b),
            (Val::Str(a), Val::Str(b)) => Ok(self.heap.get_string(a) < self.heap.get_string(b)),
            (a, b) => Err(self.table_sort_comparison_error(a, b)),
        }
    }

    /// The failure of a comparison `table.sort` performs itself, attributed
    /// to `Sort` here, where it is raised. Attribution belongs to this
    /// construction site rather than to the sort's call scope: an error from
    /// Lua the sort calls (a comparator, or a nested `table.sort` inside it)
    /// propagates with whatever attribution it already carries, and the
    /// notification `on_error` later receives is this same value. Outlined
    /// and cold so the error path stays out of `table_sort_less`, whose frame
    /// is live across every comparator re-entry.
    #[cold]
    #[inline(never)]
    fn table_sort_comparison_error(&self, a: Val, b: Val) -> Error {
        let (a, b) = (a.typ(&self.heap), b.typ(&self.heap));
        self.error(ErrorKind::TypeError(TypeError::Comparison(a, b)))
            .with_library_function(LibraryFunction::Sort)
    }

    /// Converts the value at the given index to a UTF-8 string, checking for
    /// a `__tostring` metamethod. Invalid UTF-8 bytes (from a Lua string or a
    /// `__tostring` result) are converted lossily; use
    /// [`State::tostring_bytes`] for the exact bytes. A converted result over
    /// `MAX_STRING_BYTES` is an error, checked before conversion. That check
    /// is host-side conversion, not execution, so it does not fire
    /// [`HostCallbacks::on_error`](crate::HostCallbacks::on_error); rendering
    /// errors notify as for [`State::tostring_bytes`].
    ///
    /// Guarded like [`State::call`]: refused on a poisoned State, and a
    /// panic out of `__tostring` poisons it.
    pub fn to_string_with_meta(&mut self, idx: isize) -> Result<String> {
        let raw = crate::native_call!(
            "to_string_with_meta->tostring_bytes",
            self.tostring_bytes(idx)
        )?;
        Self::lossy_utf8_checked(&raw)
    }

    /// The value's `tostring` rendering, byte-exact: what the `tostring`
    /// builtin returns, what `print` prints and what `string.format("%s")`
    /// formats. A `__tostring` result keeps its bytes whether or not they are
    /// valid UTF-8.
    ///
    /// Strings consult the shared string metatable too, as reference Lua's
    /// `tostring` does: setting `getmetatable("").__tostring` changes
    /// `tostring(s)` and `print(s)`.
    ///
    /// A handler failure leaves the stack as it was. Writes made by the
    /// handler survive the error.
    ///
    /// An invalid index is host misuse and does not fire
    /// [`HostCallbacks::on_error`](crate::HostCallbacks::on_error); every
    /// rendering failure, a failed or wrong-typed `__tostring` result
    /// included, notifies as `call`'s errors do.
    ///
    /// Guarded like [`State::call`]: refused on a poisoned State, and a
    /// panic out of `__tostring` poisons it.
    pub fn tostring_bytes(&mut self, idx: isize) -> Result<Vec<u8>> {
        self.guarded(|state| {
            crate::native_call!(
                "tostring_bytes->tostring_bytes_unguarded",
                state.tostring_bytes_unguarded(idx)
            )
        })
    }

    /// [`State::tostring_bytes`] without the poison boundary, for the
    /// standard library.
    pub(crate) fn tostring_bytes_unguarded(&mut self, idx: isize) -> Result<Vec<u8>> {
        match crate::native_call!(
            "tostring_bytes_unguarded->tostring_rendering",
            self.tostring_rendering(idx)
        )? {
            // No handler ran, so `idx` still names the same string.
            TostringRendering::Itself => Ok(self.to_bytes(idx)?.to_vec()),
            TostringRendering::Bytes(bytes) => Ok(bytes),
        }
    }

    /// The one owner of `tostring`'s rule: a value with a `__tostring`
    /// handler renders through it (a string's handler is the shared string
    /// metatable's, nil by default), so **a string bypasses `__tostring`
    /// unless the shared string metatable has one**; a string without a
    /// handler renders as itself, and any other value by its default
    /// coercion. The handler must return a string or a number.
    ///
    /// `error_value` applies the opposite rule to strings on purpose.
    ///
    /// An invalid index is host misuse and is returned unreported; every
    /// failure after it is reported here (`report_escaping`), so a host
    /// callback that handles a `tostring_bytes` error has seen it notified.
    pub(crate) fn tostring_rendering(&mut self, idx: isize) -> Result<TostringRendering> {
        let i = self.convert_idx(idx)?;
        let val = self.stack[i];
        let tostring_handler = self.tostring_handler(val);
        if !matches!(tostring_handler, Val::Nil) {
            // Handler plus receiver: net-positive by two slots until the
            // call consumes them.
            self.check_stack_space(2)
                .map_err(|error| self.report_escaping(error))?;
            self.push_unchecked(tostring_handler);
            self.push_unchecked(val);
            // A failed handler call is reported by the call's own boundary.
            crate::native_call!(
                "tostring_rendering->call_unguarded",
                self.call_unguarded(ArgCount::Fixed(1), RetCount::Fixed(1))
            )?;
            let result = self.pop_val();
            if matches!(
                result.typ(&self.heap),
                super::LuaType::String | super::LuaType::Number
            ) && let Some(bytes) = result.to_bytes_with_heap(&self.heap)
            {
                return Ok(TostringRendering::Bytes(bytes));
            }
            return Err(
                self.report_escaping(self.library_error(LibraryError::TostringMustReturnString))
            );
        }
        if matches!(val, Val::Str(_)) {
            return Ok(TostringRendering::Itself);
        }
        self.bytes_with_default_string_coercion(idx)
            .map(TostringRendering::Bytes)
            .map_err(|error| self.report_escaping(error))
    }

    /// The value `error()` raises, built from the stack value at `idx`.
    ///
    /// The opposite of `tostring`'s rule for strings: a string error value is
    /// the value itself in reference Lua, never passed through `__tostring`,
    /// so it **always bypasses** the shared string metatable's handler and
    /// keeps its exact bytes. Nil, booleans and numbers are carried as values.
    /// Tables and functions cannot outlive the State, so they carry their type
    /// and their `tostring` rendering, which may run `__tostring` and may
    /// assign a State-local identity.
    pub(crate) fn error_value(&mut self, idx: isize) -> Result<ErrorValue> {
        let val = self.at_index(idx)?;
        Ok(match val {
            Val::Nil => ErrorValue::Nil,
            Val::Bool(value) => ErrorValue::Boolean(value),
            Val::Num(number) => ErrorValue::Number(number),
            Val::Str(string) => ErrorValue::String(self.heap.get_string(string).to_vec()),
            Val::RustFn(_) | Val::Obj(_) => {
                // Read the type before any handler runs.
                let value_type = val.typ(&self.heap);
                let rendering = crate::native_call!(
                    "error_value->tostring_bytes_unguarded",
                    self.tostring_bytes_unguarded(idx)
                )?;
                ErrorValue::Object {
                    value_type,
                    rendering,
                }
            }
        })
    }

    fn metatable_tostring_field(&self, mt: Option<super::object::ObjectPtr>) -> Val {
        let key = self.meta_names.get(super::MetaName::ToString);
        mt.and_then(|ptr| self.heap.as_table_ref(ptr))
            .map_or(Val::Nil, |mt| mt.get(&key))
    }

    /// The `__tostring` field of a table's own or a string's shared metatable.
    /// Reads the State's interned key, so it neither allocates nor collects.
    fn tostring_handler(&self, val: Val) -> Val {
        let metatable_ptr = match val {
            Val::Str(_) => self.string_metatable,
            Val::Obj(ptr) => self
                .heap
                .as_table_ref(ptr)
                .and_then(super::table::Table::get_metatable),
            _ => None,
        };
        self.metatable_tostring_field(metatable_ptr)
    }

    pub(crate) fn bytes_with_default_string_coercion(&mut self, idx: isize) -> Result<Vec<u8>> {
        let i = self.convert_idx(idx)?;
        let val = self.stack[i];
        // Rust functions get the same state-local identity as closures, so
        // distinct builtins (unequal under `==`, which compares addresses)
        // render distinctly. The id map is keyed by the function value and is
        // saved state: a registered RustFn reachable at save time encodes as
        // a registry token, so its id survives save and load.
        // Strings and primitives render from the heap alone; only functions
        // and objects fall through to their pointer identity.
        if let Some(bytes) = val.to_bytes_with_heap(&self.heap) {
            return Ok(bytes);
        }
        match self.format_pointer_id(idx)? {
            Some(id) => Ok(format!("{}: 0x{id:x}", val.typ(&self.heap).as_str()).into_bytes()),
            None => Err(super::Error::internal_fault(
                InternalFault::MissingPointerIdentity,
            )),
        }
    }

    /// Returns a deterministic, state-local identity for pointer-like values
    /// (strings, objects and functions), or `None` for values that have no
    /// pointer representation. Numbers, including NaN, never enter the map.
    pub(crate) fn format_pointer_id(&mut self, idx: isize) -> Result<Option<u64>> {
        let val = self.at_index(idx)?;
        let Some(key) = PointerIdKey::from_val(val) else {
            return Ok(None);
        };
        if let Some(id) = self.format_pointer_ids.get(&key) {
            return Ok(Some(*id));
        }
        let id = self.next_format_pointer_id;
        // A snapshot can place the counter at the end of its range. Refuse a
        // fresh identity before mutating either field: wrapping would reissue
        // old ids and violate the snapshot verifier's id < next invariant.
        let next = id
            .checked_add(1)
            .ok_or_else(|| self.library_error(LibraryError::FormatPointerIdentitySpaceExhausted))?;
        self.next_format_pointer_id = next;
        self.format_pointer_ids.insert(key, id);
        Ok(Some(id))
    }

    /// Allocates a new table on the heap.
    ///
    /// Never inlined, nor is `alloc_table_with_capacity`: NEW_TABLE and
    /// NEW_TABLE_PRESIZED call them from `Frame::run`, a frame live on every
    /// native re-entry, and inlined they build the whole heap object (an
    /// empty `Table` in its wrapper) as a stack temporary of that frame
    /// before moving it into the slot map. Outlined, that storage is only on
    /// the stack while a table is being allocated.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    #[inline(never)]
    pub(super) fn alloc_table(&mut self) -> Val {
        let obj = self.allocator().alloc_table();
        Val::Obj(obj)
    }

    /// Allocates a new table with room for `capacity` hash entries; see
    /// `alloc_table` for why it is never inlined.
    #[inline(never)]
    pub(super) fn alloc_table_with_capacity(&mut self, capacity: usize) -> Val {
        let obj = self.allocator().alloc_table_with_capacity(capacity);
        Val::Obj(obj)
    }
}

#[cfg(test)]
mod sort_root_tests {
    use super::{ErrorKind, Val, reserve_sort_values};

    #[test]
    fn sort_root_reservation_reports_capacity_overflow() {
        let mut roots = Vec::<Val>::new();
        let error = reserve_sort_values(&mut roots, usize::MAX)
            .expect_err("an unrepresentable sort root list must be rejected");
        assert!(matches!(error.kind(), ErrorKind::TableGrowthFailed));
        assert!(roots.is_empty());
    }
}
