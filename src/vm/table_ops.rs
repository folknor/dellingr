//! Table operations for the Lua VM.
//!
//! This module contains methods for creating and manipulating Lua tables,
//! including metamethod-aware operations.

use super::RustFnId;
use super::lua_val::{PointerIdKey, Val};
use super::table::{TableError, TableNext};
use super::{Result, State, TypeError};
use crate::cost_model::Charge;
use crate::error::{Error, ErrorKind, ErrorValue, InternalFault, LibraryError, LibraryFunction};
use crate::instr::{ArgCount, RetCount};

/// How a value renders under `tostring`; see `State::tostring_rendering`.
pub(crate) enum TostringRendering {
    /// A string with no `__tostring` handler: the value is its own rendering,
    /// so a caller can return the original stack value without re-interning.
    Itself,
    /// A `__tostring` handler returned a string, left rooted on top of the
    /// stack: it is its own rendering, read or returned without a copy or a
    /// re-intern. The caller consumes (pops or moves) that slot.
    OnTop,
    /// The rendered bytes.
    Bytes(Vec<u8>),
}

/// What one `table.concat` element read did; see `State::concat_append_raw`.
pub(crate) enum ConcatRead {
    /// The element was a string or number and its bytes are in the output.
    Appended,
    /// The element has this type, which `table.concat` rejects.
    Invalid(super::LuaType),
    /// The raw value is nil and the metatable has `__index`: read through
    /// the handler path.
    NeedsIndex,
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

/// A sort's segment of `in_flight.rooted_values`, from the fixed `base` the
/// sort recorded when it opened the segment (`State::with_sort_segment`).
/// Every access indexes the shared Vec afresh, so no slice borrow survives a
/// comparator call: a nested sort may grow (and reallocate) the Vec above
/// this segment, and truncates back to its own base before returning.
struct RootedSortAccess {
    base: usize,
}

impl TableSortAccess for RootedSortAccess {
    fn get(&mut self, state: &mut State, index: usize) -> Result<Val> {
        Ok(state.in_flight.rooted_values[self.base + index])
    }

    fn set(
        &mut self,
        state: &mut State,
        index: usize,
        value: Val,
    ) -> std::result::Result<(), TableError> {
        state.in_flight.rooted_values[self.base + index] = value;
        Ok(())
    }
}

/// What the one heap sort needs from the thing it sorts: a safepoint, the
/// ordering of two positions, and a swap. Every `table.sort` runs the same
/// `table_sort_heap` over one of these, so equal inputs see the same
/// comparisons in the same order whichever backend holds them; a backend
/// only changes how a position is read and compared, never which positions
/// are compared.
trait HeapSortOps {
    fn heap_poll(&mut self) -> Result<()>;
    /// Whether position `a` sorts before position `b`.
    fn heap_less(&mut self, a: usize, b: usize) -> Result<bool>;
    fn heap_swap(&mut self, a: usize, b: usize) -> Result<()>;
}

/// The backend that can run Lua: values read raw through a
/// `TableSortAccess` and ordered by `State::table_sort_less`, the comparator
/// when there is one, the default comparison (and its error) otherwise.
struct LuaSortOps<'s, A> {
    state: &'s mut State,
    access: A,
    comp_idx: Option<usize>,
}

impl<A: TableSortAccess> HeapSortOps for LuaSortOps<'_, A> {
    fn heap_poll(&mut self) -> Result<()> {
        self.state.poll_interrupt()
    }

    fn heap_less(&mut self, a: usize, b: usize) -> Result<bool> {
        let a_value = self.access.get(self.state, a)?;
        let b_value = self.access.get(self.state, b)?;
        crate::native_call!(
            "heap_less->table_sort_less",
            self.state.table_sort_less(a_value, b_value, self.comp_idx)
        )
    }

    fn heap_swap(&mut self, a: usize, b: usize) -> Result<()> {
        let a_value = self.access.get(self.state, a)?;
        let b_value = self.access.get(self.state, b)?;
        self.access.set(self.state, a, b_value)?;
        self.access.set(self.state, b, a_value)?;
        Ok(())
    }
}

/// Default sort of a range that is all numbers: plain `f64 <`, exactly the
/// default comparison's number case, so NaN and `-0` land where the generic
/// path puts them. Holds the State shared, for the safepoint only.
struct NumSortOps<'s> {
    state: &'s State,
    keys: &'s mut [f64],
}

impl HeapSortOps for NumSortOps<'_> {
    fn heap_poll(&mut self) -> Result<()> {
        self.state.poll_interrupt()
    }

    fn heap_less(&mut self, a: usize, b: usize) -> Result<bool> {
        Ok(self.keys[a] < self.keys[b])
    }

    fn heap_swap(&mut self, a: usize, b: usize) -> Result<()> {
        self.keys.swap(a, b);
        Ok(())
    }
}

/// Default sort of a range that is all strings: each element's bytes are
/// resolved from the string pool once, then compared as byte slices, the
/// default comparison's string case. The default sort cannot run Lua or
/// collect, and the source table holds every string, so the borrowed bytes
/// stay valid for the whole sort.
struct StrSortOps<'k, 's> {
    state: &'s State,
    keys: &'k mut [(&'s [u8], Val)],
}

impl HeapSortOps for StrSortOps<'_, '_> {
    fn heap_poll(&mut self) -> Result<()> {
        self.state.poll_interrupt()
    }

    fn heap_less(&mut self, a: usize, b: usize) -> Result<bool> {
        Ok(self.keys[a].0 < self.keys[b].0)
    }

    fn heap_swap(&mut self, a: usize, b: usize) -> Result<()> {
        self.keys.swap(a, b);
        Ok(())
    }
}

fn table_sort_heap<O: HeapSortOps>(ops: &mut O, n: usize) -> Result<()> {
    // Interrupt safepoints: every heapify root, sortdown step, sift level
    // and reversal swap. The default comparator calls no Lua, so without
    // them the O(n log n) comparisons would run uninterruptible.
    // safepoint: `table_sort_sift_down` polls on entry and per level.
    for root in (0..n / 2).rev() {
        crate::native_call!(
            "table_sort_heap->table_sort_sift_down:heapify",
            table_sort_sift_down(ops, root, n)
        )?;
    }
    // safepoint: `heap_poll` (every backend's is `State::poll_interrupt`).
    for end in (1..n).rev() {
        ops.heap_poll()?;
        ops.heap_swap(0, end)?;
        crate::native_call!(
            "table_sort_heap->table_sort_sift_down:sortdown",
            table_sort_sift_down(ops, 0, end)
        )?;
    }
    // safepoint: `heap_poll`, as above.
    for index in 0..n / 2 {
        // Distinct positions: `index < n / 2` puts `n - 1 - index` past it.
        ops.heap_poll()?;
        ops.heap_swap(index, n - 1 - index)?;
    }
    Ok(())
}

fn table_sort_sift_down<O: HeapSortOps>(ops: &mut O, mut root: usize, end: usize) -> Result<()> {
    // safepoint: `heap_poll` per level (`State::poll_interrupt` in every backend).
    loop {
        ops.heap_poll()?;
        let left = root * 2 + 1;
        if left >= end {
            return Ok(());
        }
        let mut smallest = root;
        if crate::native_call!(
            "table_sort_sift_down->heap_less:left",
            ops.heap_less(left, smallest)
        )? {
            smallest = left;
        }
        let right = left + 1;
        if right < end
            && crate::native_call!(
                "table_sort_sift_down->heap_less:right",
                ops.heap_less(right, smallest)
            )?
        {
            smallest = right;
        }
        if smallest == root {
            return Ok(());
        }
        ops.heap_swap(root, smallest)?;
        root = smallest;
    }
}

impl State {
    /// Creates a new empty table and pushes it onto the stack.
    pub fn new_table(&mut self) -> Result<()> {
        self.check_usable()?;
        self.push_new_table()
    }

    /// [`State::new_table`] without the entry check, for the dispatch loop
    /// and library setup.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(crate) fn push_new_table(&mut self) -> Result<()> {
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

    pub(crate) fn set_table_str_key_rust_fn(
        &mut self,
        table_idx: isize,
        name: &str,
        function: RustFnId,
    ) -> Result<()> {
        self.set_table_str_key_value(table_idx, name, Val::RustFn(function))
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
    /// Validation and dispatch preflight failures leave the stack unchanged,
    /// as does an `__index` function call refused before entry (stack or call
    /// depth, or a pending interrupt). An error from calling an `__index` function consumes the key and clears
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
        self.check_usable()?;
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
        self.check_usable()?;
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

    /// Does the equivalent of `t[k] = v`, where `t` is the value at the given
    /// valid index, `k` is the value at the top of the stack minus 1, and `v`
    /// is the value at the top of the stack. Pops both the key and the value.
    /// As in Lua, this function may trigger a metamethod for the "newindex"
    /// event: it stores exactly as a script's `t[k] = v` does, through
    /// `__newindex` handler tables and functions, under the same metamethod
    /// depth rule. The receiver must be a table, as in a script.
    ///
    /// A nil or NaN key is passed to a `__newindex` function like any other
    /// key and refused only by a raw store.
    ///
    /// Every failure before a `__newindex` function is entered leaves the
    /// stack unchanged: a bad index, too few operands, a non-table receiver,
    /// an invalid key for a raw store, a handler that can be neither indexed
    /// nor called, the metamethod depth limit, a refused table growth, and a
    /// handler call refused before entry, including by a pending interrupt. An error from an entered handler
    /// consumes the key and value and clears handler temporaries, preserving
    /// every earlier stack slot. Writes made by the handler survive the error.
    ///
    /// An invalid index or missing operands are host misuse and do not fire
    /// [`HostCallbacks::on_error`](crate::HostCallbacks::on_error); a
    /// receiver that is not a table, and every failure of the store itself,
    /// notify as `call`'s errors do.
    ///
    /// Charges no cost, like [`State::get_table`] and [`State::set_table_raw`].
    /// Guarded like [`State::call`]: refused on a poisoned State, and a panic
    /// out of a metamethod poisons it.
    pub fn set_table(&mut self, i: isize) -> Result<()> {
        self.guarded(|state| {
            crate::native_call!(
                "set_table->set_table_unguarded",
                state.set_table_unguarded(i)
            )
        })
    }

    /// [`State::set_table`] without the poison boundary, for the standard
    /// library and other VM-internal callers. The key and value stay on the
    /// stack as GC roots throughout dispatch, below any handler window, so a
    /// failure before handler entry has nothing to restore.
    // This frame survives __newindex re-entry; instrumentation would add to
    // every level of the bounded native stack.
    pub(crate) fn set_table_unguarded(&mut self, i: isize) -> Result<()> {
        let idx = self
            .table_operand_below_top(i, 2)
            .map_err(|error| self.report_index_receiver_error(error))?;
        let top = self.stack.len();
        let key = self.stack[top - 2];
        let val = self.stack[top - 1];
        let mut local_cost = 0;
        let mut handler_entered = false;
        let ptr = self.stack[idx]
            .as_object_ptr()
            .expect("table_operand_below_top validated the table");
        let tbl = self
            .heap
            .as_table(ptr)
            .expect("table_operand_below_top validated the table");
        // An existing key is written through the slot its lookup found.
        let stored = if let Some((slot, _)) = tbl.find_slot(&key) {
            if matches!(val, Val::Nil) {
                tbl.remove_slot(slot);
            } else {
                tbl.store_slot(slot, val);
            }
            Ok(None)
        } else {
            self.store_new_key(ptr, key, val, &mut local_cost)
        };
        let result = match stored {
            Ok(Some(units)) => {
                let called = crate::native_call!(
                    "set_table_unguarded->call_unguarded",
                    self.call_unguarded(
                        (
                            ArgCount::Fixed(3),
                            super::eval::StoreEntry(&mut handler_entered),
                        ),
                        RetCount::Fixed(0),
                    )
                );
                self.in_flight.metamethod_depth -= units;
                called
            }
            Ok(None) => Ok(()),
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => {
                self.stack.truncate(top - 2);
                Ok(())
            }
            Err(error) => {
                // Before entry the operands were never consumed: drop only a
                // pushed window. After entry the store is spent.
                self.stack
                    .truncate(if handler_entered { top - 2 } else { top });
                Err(self.report_escaping(error))
            }
        }
    }

    /// Returns the next key-value pair from a table, for use with `pairs`.
    /// Takes the table index and pops the key from the stack.
    /// Pushes the next key and value onto the stack (or just nil if done).
    /// Every failure (bad index, non-table, invalid key, stack overflow) is
    /// detected before the key is popped, so a host loop can retry or resume.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn table_next(&mut self, table_idx: isize) -> Result<bool> {
        self.check_usable()?;
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
        self.check_usable()?;
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

    /// The table at stack index `idx` and its metatable, for the library's
    /// metamethod-aware sequence operations. A non-table is a length type
    /// error; the library checks the argument type first, so a string never
    /// reaches here and its shared metatable is never consulted.
    fn sequence_operand(&self, idx: isize) -> Result<(Val, Option<super::object::ObjectPtr>)> {
        let val = self.at_index(idx)?;
        let table = val
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .ok_or_else(|| self.type_error(TypeError::Length(val.typ(&self.heap))))?;
        Ok((val, table.get_metatable()))
    }

    /// Lua 5.4's `luaL_len` up to the handler call, for the table at `idx`:
    /// the raw border when its metatable has no `__len` (or it has none),
    /// with the stack unchanged; otherwise `None`, with the window
    /// `[handler, t, t]` pushed for a call with two arguments and one result.
    /// The window is pushed by the routine the VM's `#` uses, so a refusal
    /// at the stack cap leaves the stack unchanged and reads the same on
    /// both paths. Looking the handler up takes no stack slots: only a
    /// handler call needs room. Any non-nil handler counts; whether it is
    /// callable is call resolution's question.
    pub(crate) fn length_or_push_len_call(&mut self, idx: isize) -> Result<Option<usize>> {
        let (val, mt) = self.sequence_operand(idx)?;
        let handler = self.metatable_field(mt, super::MetaName::Len);
        if matches!(handler, Val::Nil) {
            return Ok(Some(self.table_len_of(val)));
        }
        self.push_len_window(handler, val)?;
        Ok(None)
    }

    /// Whether the table at `idx` has a metatable defining `__len`,
    /// `__index` or `__newindex`: the events that make Lua 5.4's sequence
    /// functions (lua_geti/lua_seti/luaL_len) differ from raw access. A raw
    /// heap read, so it needs no stack room.
    pub(crate) fn has_sequence_metamethods(&self, idx: isize) -> Result<bool> {
        use super::MetaName::{Index, Len, NewIndex};
        let (_, mt) = self.sequence_operand(idx)?;
        if mt.is_none() {
            return Ok(false);
        }
        Ok([Len, Index, NewIndex]
            .into_iter()
            .any(|name| !matches!(self.metatable_field(mt, name), Val::Nil)))
    }

    /// The raw value of `t[key]` for the table at `idx`, or `None` when that
    /// value is nil and the table's metatable has an `__index` at this moment:
    /// only then does Lua 5.4's `lua_geti` consult a handler. The sequence
    /// readers (`table.concat`, `unpack`) take the stack-borne
    /// `get_table_unguarded` path only for `None`, so an ordinary element
    /// costs one table read and no key push. The metatable is looked up per
    /// call, so a handler that sets or clears one mid-loop is seen by the
    /// next element. A heap read: it neither pushes, allocates nor collects.
    fn sequence_raw_get(&self, idx: isize, key: i64) -> Result<Option<Val>> {
        let (val, mt) = self.sequence_operand(idx)?;
        let table = val
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .expect("sequence_operand validated the table");
        // The key is the f64 the stack-borne path would push.
        let value = table.get(&Val::Num(key as f64));
        if matches!(value, Val::Nil)
            && !matches!(self.metatable_field(mt, super::MetaName::Index), Val::Nil)
        {
            return Ok(None);
        }
        Ok(Some(value))
    }

    /// `table.pack`'s body: pushes a new table holding the callback's
    /// arguments at keys `1..=n` and `n` at `"n"`, leaving the arguments in
    /// place below it. Each argument is stored straight into the table, in
    /// key order, as raw stores of key/value pushes did, so a nil argument
    /// occupies no slot and the table's shape is unchanged (a `set_list`
    /// build would keep nil positional slots and move `#` on tables with
    /// holes). The elided pushes keep their effect on failure: the first
    /// key/value pair's two slot checks run before any store (later pairs
    /// reuse the same slots), and `"n"` is interned between its key's and
    /// value's checks, so a refusal at the stack cap and the collection point
    /// of that intern fall where the pushes put them.
    pub(crate) fn pack_arguments(&mut self) -> Result<()> {
        let count = self.get_top();
        let first = self.stack.len() - count;
        self.new_table()?;
        let len = self.stack.len();
        let ptr = self.stack[len - 1]
            .as_object_ptr()
            .expect("new_table pushed a table");
        if count > 0 {
            self.check_stack_slot_at(len)?;
            self.check_stack_slot_at(len + 1)?;
            // bounded: one step per argument, at most MAX_ARGUMENTS.
            for (slot, key) in (first..first + count).zip(1_u32..) {
                let value = self.stack[slot];
                self.heap
                    .as_table(ptr)
                    .expect("new_table pushed a table")
                    .insert(Val::Num(f64::from(key)), value)?;
            }
        }
        self.check_stack_slot_at(len)?;
        let key = self.alloc_string(b"n")?;
        self.check_stack_slot_at(len + 1)?;
        self.heap
            .as_table(ptr)
            .expect("new_table pushed a table")
            .insert(key, Val::Num(count as f64))?;
        Ok(())
    }

    /// `table.concat`'s read of element `key` of the table at `idx` into
    /// `out`, when no `__index` handler is needed (see `sequence_raw_get`).
    /// [`ConcatRead::NeedsIndex`] appends nothing: the caller reads the
    /// element through `get_table_unguarded` and appends it with
    /// [`State::concat_append_top`].
    pub(crate) fn concat_append_raw(
        &self,
        idx: isize,
        key: i64,
        out: &mut Vec<u8>,
    ) -> Result<ConcatRead> {
        let Some(val) = self.sequence_raw_get(idx, key)? else {
            return Ok(ConcatRead::NeedsIndex);
        };
        Ok(match self.append_concat_value(val, out)? {
            Ok(()) => ConcatRead::Appended,
            Err(typ) => ConcatRead::Invalid(typ),
        })
    }

    /// Appends the value on top of the stack (a handler's result, rooted
    /// there while its bytes are copied) as [`State::concat_append_raw`]
    /// would. The stack is unchanged.
    pub(crate) fn concat_append_top(&self, out: &mut Vec<u8>) -> Result<ConcatRead> {
        let val = self.at_index(-1)?;
        Ok(match self.append_concat_value(val, out)? {
            Ok(()) => ConcatRead::Appended,
            Err(typ) => ConcatRead::Invalid(typ),
        })
    }

    /// Pushes element `key` of the table at `idx` for `unpack` and returns
    /// true, or returns false with nothing pushed when the element needs an
    /// `__index` handler (see `sequence_raw_get`). The push makes the same
    /// one-slot check the key push of the handler path makes, so a refusal
    /// at the stack cap comes at the same element on both paths.
    pub(crate) fn push_sequence_raw(&mut self, idx: isize, key: i64) -> Result<bool> {
        match self.sequence_raw_get(idx, key)? {
            Some(val) => {
                self.push_val(val)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Appends `table.concat`'s rendering of `val` to `out`: a string's bytes
    /// straight from the pool, a number through `number_to_text`, with no
    /// owned copy in between. Any other type comes back as `Err(type)` for
    /// the caller's invalid-value error; the output-size limit is checked
    /// before anything is appended. Neither allocates on the heap nor
    /// collects, so a raw-read element needs no rooting.
    fn append_concat_value(
        &self,
        val: Val,
        out: &mut Vec<u8>,
    ) -> Result<std::result::Result<(), super::LuaType>> {
        let text;
        let bytes: &[u8] = match val {
            Val::Str(s) => self.heap.get_string(s),
            Val::Num(n) => {
                text = crate::number_text::number_to_text(n);
                text.as_bytes()
            }
            other => return Ok(Err(other.typ(&self.heap))),
        };
        let next = crate::limits::checked_string_growth(out.len(), bytes.len())?;
        out.reserve(next - out.len());
        out.extend_from_slice(bytes);
        Ok(Ok(()))
    }

    /// Push the `__len` call window `[handler, val, val]` after one
    /// preflight of all three slots, so a refusal pushes nothing.
    pub(super) fn push_len_window(&mut self, handler: Val, val: Val) -> Result<()> {
        self.check_stack_space(3)?;
        self.push_unchecked(handler);
        self.push_unchecked(val);
        self.push_unchecked(val);
        Ok(())
    }

    fn table_len_of(&self, val: Val) -> usize {
        val.as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .expect("sequence_operand validated the table")
            .array_len()
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
        self.check_usable()?;
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
        self.check_usable()?;
        self.pop_metatable_onto(table_idx)
    }

    /// [`State::set_metatable_of`] without the entry check, for library
    /// setup.
    pub(crate) fn pop_metatable_onto(&mut self, table_idx: isize) -> Result<()> {
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
        self.check_usable()?;
        // Resolve against the entry stack, including the value to be popped.
        // This keeps negative indices relative to the caller's visible top and
        // validates the table before mutating the stack.
        let idx = self.table_operand_below_top(table_idx, 1)?;
        let len = self.table_array_len_at(idx);
        if !(1..=len + 1).contains(&pos) {
            return Err(self.library_error(LibraryError::PositionOutOfBounds));
        }
        self.insert_top_at_len(idx, pos, len)?;
        self.pop_val();
        Ok(())
    }

    /// `table.insert`'s raw store: inserts the top value at `pos` of the
    /// table at stack index `table_idx`, leaving the value on the stack. The
    /// library has type-checked the table and validated `pos` against `len`,
    /// the border it computed with `table_len`, so the border is computed
    /// once per call rather than again here and in the storage shift.
    pub(crate) fn sequence_insert(
        &mut self,
        table_idx: isize,
        pos: usize,
        len: usize,
    ) -> Result<()> {
        let idx = self.convert_idx(table_idx)?;
        self.insert_top_at_len(idx, pos, len)
    }

    fn insert_top_at_len(&mut self, idx: usize, pos: usize, len: usize) -> Result<()> {
        let value = self.stack[self.stack.len() - 1];
        self.stack[idx]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table(ptr))
            .expect("the caller validated the table")
            .array_insert_at_len(pos, len, value)?;
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
        self.check_usable()?;
        let idx = self.table_operand_below_top(table_idx, 0)?;
        let len = self.table_array_len_at(idx);
        let valid = (1..=len + 1).contains(&pos) || (len == 0 && pos == 0);
        if !valid {
            return Err(self.library_error(LibraryError::PositionOutOfBounds));
        }
        self.remove_push_at_len(idx, pos, len)
    }

    /// `table.remove`'s raw removal: removes `pos` of the table at stack
    /// index `table_idx` and pushes the removed value. The library has
    /// type-checked the table and validated `pos` against `len`, the border
    /// it computed with `table_len`.
    pub(crate) fn sequence_remove(
        &mut self,
        table_idx: isize,
        pos: usize,
        len: usize,
    ) -> Result<()> {
        let idx = self.convert_idx(table_idx)?;
        self.remove_push_at_len(idx, pos, len)
    }

    fn remove_push_at_len(&mut self, idx: usize, pos: usize, len: usize) -> Result<()> {
        // Reserve the result slot *before* mutating the table: failing after
        // the removal would discard the value with no way to return it.
        self.check_stack_space(1)?;
        let removed = self.stack[idx]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table(ptr))
            .expect("the caller validated the table")
            .array_remove_at_len(pos, len)?;
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
        let len: i64 = crate::saturate::saturating_cast(array_len);
        let n = self.checked_table_sort_len(len)?;

        // Heap sort remains bounded and deterministic even when a Lua
        // comparator is inconsistent. Unlike reference quicksort, it does not
        // diagnose that incidental invalid-order case.
        if has_comp {
            // A comparator can re-enter Lua and force a collection, so the
            // values sort in a segment of the rooted buffer, copied there
            // once, rooted as a whole for the entire sort (not just the pair
            // being compared) and written back from there. `src/vm/tests.rs`
            // covers a comparator that clears the source table before
            // collecting.
            //
            // Each comparison is a nested invocation through
            // `call_unguarded`. Running the comparator on the calling
            // dispatch loop's frame stack instead (a resumable heap sort, or
            // a private bytecode driver looping CALL over a sort step) was
            // considered and not pursued: on bygg, sorting 2000 numbers with
            // `function(x, y) return x < y end` measured within about 20
            // percent of the same number of plain Lua-to-Lua calls of that
            // comparator, heap work included, so the nested invocation
            // leaves little to win against a new frame kind in the dispatch
            // loop's return path.
            return self.with_sort_segment(|state, base| {
                state
                    .heap
                    .as_table_ref(table_ptr)
                    .expect("validated object pointer is a table")
                    .append_array_to(&mut state.in_flight.rooted_values)?;
                // Charged after the copy, as the default path charges after
                // its detached copy, and before the comparator runs or the
                // table is mutated, so an exhausted budget blocks the sort.
                state.charge(Charge::TableSort, n.max(1) as u64)?;
                if n == 0 {
                    return Ok(0);
                }
                crate::native_call!(
                    "table_sort->table_sort_rooted",
                    state.table_sort_rooted(base, n, true)
                )?;
                // Look the table up again: the comparator may have replaced
                // the argument slot's contents.
                let obj_ptr = state.stack[idx].as_object_ptr();
                let sorted = &state.in_flight.rooted_values[base..base + n];
                match obj_ptr.and_then(|ptr| state.heap.as_table(ptr)) {
                    Some(t) => {
                        t.set_array(sorted)?;
                        Ok(n)
                    }
                    None => Err(state.type_error(TypeError::TableIndex(typ))),
                }
            });
        }

        let mut arr = self
            .heap
            .as_table_ref(table_ptr)
            .expect("validated object pointer is a table")
            .get_array()?;

        // Charge cost BEFORE mutating the table, so an exhausted budget
        // blocks the sort rather than letting it complete and only then
        // failing the charge. `arr` is a detached copy here, so nothing has
        // been mutated yet. Empty sorts still cost 1.
        self.charge(Charge::TableSort, n.max(1) as u64)?;

        // The default comparison only reads numbers and already-interned
        // string bytes: it cannot call Lua or trigger GC (its failure
        // allocates only the Rust error, not VM objects), and the source
        // table holds every element until writeback, so `arr` needs no root.
        crate::native_call!(
            "table_sort->table_sort_default",
            self.table_sort_default(&mut arr)
        )?;
        // A successful default sort of two or more elements compared every
        // one of them, so the range is all numbers or all strings in live
        // window slots: the writeback permutes those slots and touches no
        // storage metadata. A single element is already sorted.
        if arr.len() >= 2 {
            self.heap
                .as_table(table_ptr)
                .expect("the default sort cannot reshape the table")
                .overwrite_live_prefix(&arr);
        }
        Ok(n)
    }

    /// Run `f` with a sort segment open: `f` receives the segment's base,
    /// the current length of `in_flight.rooted_values`, and appends the
    /// values to sort from there. Everything from the base is dropped when
    /// `f` returns, on success, error or panic (which poisons, as in
    /// `with_rooted_values`), so nested sorts nest their segments LIFO.
    pub(crate) fn with_sort_segment<R>(
        &mut self,
        f: impl FnOnce(&mut Self, usize) -> Result<R>,
    ) -> Result<R> {
        self.with_rooted_values(&[], |state| {
            let base = state.in_flight.rooted_values.len();
            f(state, base)
        })
    }

    /// Append the value on top of the stack to the open sort segment and pop
    /// it. The value stays on the stack, rooted, until it is in the
    /// segment. Growth is amortized and fallible (`TableGrowthFailed`).
    pub(crate) fn pop_into_sort_segment(&mut self) -> Result<()> {
        let value = *self.stack.last().expect("a value to move into the segment");
        self.in_flight
            .rooted_values
            .try_reserve(1)
            .map_err(|_| Error::without_location(ErrorKind::TableGrowthFailed))?;
        self.in_flight.rooted_values.push(value);
        self.pop_val();
        Ok(())
    }

    /// Push element `index` of the sort segment at `base` onto the stack.
    pub(crate) fn push_sort_segment_value(&mut self, base: usize, index: usize) -> Result<()> {
        let value = self.in_flight.rooted_values[base + index];
        self.check_stack_slot()?;
        self.push_unchecked(value);
        Ok(())
    }

    /// Heap-sort the `n` values of the sort segment at `base`, through the
    /// comparator at frame index 2 when `has_comp`, the default comparison
    /// otherwise.
    pub(crate) fn table_sort_rooted(
        &mut self,
        base: usize,
        n: usize,
        has_comp: bool,
    ) -> Result<()> {
        let comp_idx = has_comp.then(|| self.convert_idx(2)).transpose()?;
        let mut ops = LuaSortOps {
            state: self,
            access: RootedSortAccess { base },
            comp_idx,
        };
        crate::native_call!(
            "table_sort_rooted->table_sort_heap",
            table_sort_heap(&mut ops, n)
        )
    }

    /// The generic default sort of a detached range that is neither all
    /// numbers nor all strings: no comparator, so it cannot re-enter Lua.
    fn table_sort_values(&mut self, values: &mut [Val]) -> Result<()> {
        let n = values.len();
        let mut ops = LuaSortOps {
            state: self,
            access: VecSortAccess { values },
            comp_idx: None,
        };
        crate::native_call!(
            "table_sort_values->table_sort_heap",
            table_sort_heap(&mut ops, n)
        )
    }

    /// The raw sort with no comparator. A range that is all numbers or all
    /// strings sorts on its keys alone (`NumSortOps`, `StrSortOps`); any
    /// other range runs the generic backend, which raises the comparison
    /// error for the first incomparable pair the heap meets, as before. All
    /// three run the same heap sort with the same comparison results, so the
    /// permutation does not depend on which one ran.
    ///
    /// The keys are sorted detached, not on the table's storage: an
    /// interrupt or comparison error then leaves the table untouched, as on
    /// every other sort path. Sorting the storage in place would save only
    /// the linear copy, which is not where the time goes.
    fn table_sort_default(&mut self, values: &mut [Val]) -> Result<()> {
        let n = values.len();
        if n >= 2 && values.iter().all(|v| matches!(v, Val::Num(_))) {
            let mut keys = Vec::new();
            keys.try_reserve_exact(n)
                .map_err(|_| Error::without_location(ErrorKind::TableGrowthFailed))?;
            keys.extend(values.iter().map(|v| match v {
                Val::Num(x) => *x,
                _ => unreachable!("the range was checked to hold only numbers"),
            }));
            crate::native_call!(
                "table_sort_default->table_sort_heap:numbers",
                table_sort_heap(
                    &mut NumSortOps {
                        state: self,
                        keys: &mut keys
                    },
                    n
                )
            )?;
            // bounded: one copy per element of a range below `TABLE_SORT_MAX_LEN`.
            for (slot, key) in values.iter_mut().zip(keys) {
                *slot = Val::Num(key);
            }
            return Ok(());
        }
        if n >= 2 && values.iter().all(|v| matches!(v, Val::Str(_))) {
            let mut keys = Vec::new();
            keys.try_reserve_exact(n)
                .map_err(|_| Error::without_location(ErrorKind::TableGrowthFailed))?;
            keys.extend(values.iter().map(|v| match v {
                Val::Str(s) => (self.heap.get_string(*s), *v),
                _ => unreachable!("the range was checked to hold only strings"),
            }));
            crate::native_call!(
                "table_sort_default->table_sort_heap:strings",
                table_sort_heap(
                    &mut StrSortOps {
                        state: self,
                        keys: &mut keys
                    },
                    n
                )
            )?;
            // bounded: one copy per element of a range below `TABLE_SORT_MAX_LEN`.
            for (slot, (_, value)) in values.iter_mut().zip(keys) {
                *slot = value;
            }
            return Ok(());
        }
        crate::native_call!(
            "table_sort_default->table_sort_values",
            self.table_sort_values(values)
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
        self.guarded(|state| {
            let raw = crate::native_call!(
                "to_string_with_meta->tostring_bytes_unguarded",
                state.tostring_bytes_unguarded(idx)
            )?;
            Self::lossy_utf8_from_vec(raw)
        })
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
    /// standard library and for [`State::to_string_with_meta`], which runs
    /// it inside its own guard rather than calling the guarded entry.
    pub(crate) fn tostring_bytes_unguarded(&mut self, idx: isize) -> Result<Vec<u8>> {
        match crate::native_call!(
            "tostring_bytes_unguarded->tostring_rendering",
            self.tostring_rendering(idx)
        )? {
            // No handler ran, so `idx` still names the same string.
            TostringRendering::Itself => Ok(self.to_bytes(idx)?.to_vec()),
            TostringRendering::OnTop => {
                let bytes = self.to_bytes(-1)?.to_vec();
                self.pop(1)?;
                Ok(bytes)
            }
            TostringRendering::Bytes(bytes) => Ok(bytes),
        }
    }

    /// The one owner of `tostring`'s rule: a value with a `__tostring`
    /// handler renders through it (a string's handler is the shared string
    /// metatable's, nil by default), so **a string bypasses `__tostring`
    /// unless the shared string metatable has one**; a string without a
    /// handler renders as itself, and any other value by its default
    /// coercion. The handler must return a string or a number. A string
    /// result stays rooted on top of the stack (`TostringRendering::OnTop`)
    /// for the caller to consume; a number is popped and rendered.
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
            if self.typ(-1) == super::LuaType::String {
                return Ok(TostringRendering::OnTop);
            }
            let result = self.pop_val();
            if result.typ(&self.heap) == super::LuaType::Number
                && let Some(bytes) = result.to_bytes_with_heap(&self.heap)
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
            // A NaN leaves as the canonical NaN, as from `State::to_number`.
            Val::Num(number) => ErrorValue::Number(crate::numeral::canonical_nan(number)),
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

    /// The raw `name` field of the metatable `mt`, nil when there is none.
    /// Choosing whose metatable (a table's own, the strings' shared one) is
    /// the caller's. Reads the State's interned key, so it neither pushes,
    /// allocates nor collects.
    pub(super) fn metatable_field(
        &self,
        mt: Option<super::object::ObjectPtr>,
        name: super::MetaName,
    ) -> Val {
        let key = self.meta_names.get(name);
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
        self.metatable_field(metatable_ptr, super::MetaName::ToString)
    }

    pub(crate) fn bytes_with_default_string_coercion(&mut self, idx: isize) -> Result<Vec<u8>> {
        let i = self.convert_idx(idx)?;
        let val = self.stack[i];
        // Rust functions get the same state-local identity as closures, so
        // distinct builtins (unequal under `==`, which compares function
        // table entries) render distinctly. The id map is keyed by the function value and is
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
    use super::{LuaSortOps, NumSortOps, StrSortOps, Val, VecSortAccess, table_sort_heap};
    use crate::vm::State;

    /// The permutation the generic backend's default comparison produces.
    fn generic_order(state: &mut State, values: &[Val]) -> Vec<Val> {
        let mut out = values.to_vec();
        let n = out.len();
        let mut ops = LuaSortOps {
            state,
            access: VecSortAccess { values: &mut out },
            comp_idx: None,
        };
        table_sort_heap(&mut ops, n).expect("homogeneous ranges sort");
        out
    }

    /// Bits of a number range, so `-0` against `0` and NaN positions count.
    fn bits(values: &[Val]) -> Vec<u64> {
        values
            .iter()
            .map(|v| match v {
                Val::Num(x) => x.to_bits(),
                _ => panic!("expected a number"),
            })
            .collect()
    }

    // The number and string backends must reach the generic backend's exact
    // permutation, including where NaN, `0` and `-0` and equal keys land:
    // the order is replay-visible.
    #[test]
    fn specialized_default_sorts_match_the_generic_permutation() {
        let mut state = State::new();
        let mut seed: u64 = 7;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            seed >> 33
        };
        for n in 0..70 {
            let numbers: Vec<Val> = (0..n)
                .map(|_| {
                    Val::Num(match next() % 6 {
                        0 => f64::NAN,
                        1 => 0.0,
                        2 => -0.0,
                        _ => (next() % 9) as f64,
                    })
                })
                .collect();
            let expected = generic_order(&mut state, &numbers);
            let mut keys: Vec<f64> = bits(&numbers).into_iter().map(f64::from_bits).collect();
            table_sort_heap(
                &mut NumSortOps {
                    state: &state,
                    keys: &mut keys,
                },
                n,
            )
            .expect("number sort");
            let got: Vec<u64> = keys.iter().map(|k| k.to_bits()).collect();
            assert_eq!(got, bits(&expected), "numbers, n = {n}");

            let strings: Vec<Val> = (0..n)
                .map(|_| {
                    let text = ["", "a", "ab", "b", "\u{0}", "aa"][(next() % 6) as usize];
                    state.alloc_string(text).expect("intern")
                })
                .collect();
            let expected = generic_order(&mut state, &strings);
            let mut keys: Vec<(&[u8], Val)> = strings
                .iter()
                .map(|v| match v {
                    Val::Str(s) => (state.heap.get_string(*s), *v),
                    _ => panic!("expected a string"),
                })
                .collect();
            table_sort_heap(
                &mut StrSortOps {
                    state: &state,
                    keys: &mut keys,
                },
                n,
            )
            .expect("string sort");
            let got: Vec<Val> = keys.into_iter().map(|(_, v)| v).collect();
            assert_eq!(got, expected, "strings, n = {n}");
        }
    }
}
