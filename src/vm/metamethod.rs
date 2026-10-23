//! Metamethod handling for __index and __newindex.
//!
//! This module handles Lua metamethods that intercept table access and assignment.
//! Metamethods allow tables to define custom behavior when:
//! - A key is accessed but doesn't exist in the table (__index)
//! - A new key is being assigned to the table (__newindex)

use super::MetaName;
use super::Result;
use super::State;
use super::frame::Frame;
use super::lua_val::Val;
use crate::error::ErrorKind;
use crate::instr::{ArgCount, RetCount};

use crate::limits::MAX_METAMETHOD_DEPTH;

/// Where an `__index` walk starts (`State::walk_index_tables`).
#[derive(Clone, Copy)]
pub(super) enum IndexStart {
    /// The receiver is a table: its raw lookup first, then its `__index`.
    Receiver,
    /// The receiver's `__index` handler, already read (a string receiver's).
    Handler(Val),
}

/// Where an `__index` lookup goes once its table steps are done.
pub(super) enum IndexTarget {
    /// Resolved: the result was pushed, or the lookup failed.
    Done(Result<()>),
    /// A function handler: its unit is taken and its call window
    /// `[handler, receiver, key]` pushed, for a call with two arguments and
    /// one result.
    Call,
    /// A string handler, its unit taken: index that string again.
    Str(Val),
}

impl State {
    /// Run one metamethod step while enforcing the shared recursion limit.
    ///
    /// Every `__newindex` step counts, and every `__index` step that nests:
    /// a table that continues to its own `__index`, a string handler, or a
    /// function handler. A terminal `__index` table step, one answered from
    /// the table's raw contents, takes no unit on any route
    /// (`State::handle_index_metamethod`). Function handlers count even
    /// though they are calls, so a memoising `__index` that recurses through
    /// its own table stops within `MAX_METAMETHOD_DEPTH` levels; a function
    /// handler is also a call made from Rust, so the lower
    /// `MAX_NATIVE_CALL_DEPTH` refuses that recursion first. That is
    /// deliberate and matches reference Lua, whose C call limit bounds the
    /// same recursion near 200. A `__call` function handler is exempt (see
    /// `State::begin_callable_call`) because guarding it would also cap
    /// ordinary recursion through callable objects, which reference Lua does
    /// not limit this way.
    ///
    /// The depth counter is restored for every VM error path. A Rust panic
    /// leaves the State unusable, like other interrupted VM execution paths.
    pub(super) fn with_metamethod_depth<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        self.take_metamethod_unit()?;
        let result = operation(self);
        self.metamethod_depth -= 1;
        result
    }

    /// Take one unit of the metamethod depth, or refuse at the limit. The
    /// caller returns it.
    fn take_metamethod_unit(&mut self) -> Result<()> {
        #[cfg(not(feature = "fault-injection"))]
        let injected = false;
        #[cfg(feature = "fault-injection")]
        let injected = self.inject_fault("metamethod", self.metamethod_depth as usize, true);
        if injected || self.metamethod_depth >= MAX_METAMETHOD_DEPTH {
            return Err(self.error(ErrorKind::MetamethodDepthExceeded {
                depth: self.metamethod_depth,
                limit: MAX_METAMETHOD_DEPTH,
            }));
        }
        self.metamethod_depth += 1;
        Ok(())
    }

    /// Internal helper for table access with __index support.
    pub(super) fn get_table_with_key(
        &mut self,
        idx: usize,
        key: Val,
        local_cost: &mut u64,
    ) -> Result<()> {
        // native-call: get_table_with_key->get_table_with_key_tracking_call
        self.get_table_with_key_tracking_call(idx, key, local_cost, &mut false)
    }

    /// Like bytecode lookup, but records the commit point for host reads:
    /// only entering a user function can consume the key on failure.
    pub(super) fn get_table_with_key_tracking_call(
        &mut self,
        idx: usize,
        key: Val,
        local_cost: &mut u64,
        handler_called: &mut bool,
    ) -> Result<()> {
        let receiver = self.stack[idx];
        if receiver
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .is_none()
        {
            return Err(self.index_handler_error(receiver));
        }
        // native-call: get_table_with_key_tracking_call->handle_index_metamethod
        self.handle_index_metamethod(receiver, key, local_cost, handler_called)
    }

    /// The error for an `__index` handler that can be neither indexed nor
    /// called. Kept out of line, away from the frames that stay live across
    /// a handler's re-entry.
    #[cold]
    #[inline(never)]
    fn index_handler_error(&self, handler: Val) -> super::Error {
        self.type_error(super::TypeError::TableIndex(handler.typ(&self.heap)))
    }

    /// Resolve `receiver[key]` for a table receiver, through its `__index`
    /// if the raw lookup misses, pushing the result.
    ///
    /// This is where the depth rule for `__index` lives. A table step is
    /// terminal when the table holds the key raw or has no `__index` of its
    /// own to continue to: it calls nothing and recurses nowhere, so it
    /// takes no metamethod-depth unit. A table step that continues holds one
    /// until the lookup ends, and so does a function or string handler. A
    /// handler that can be neither indexed nor called is rejected without
    /// one, since it recurses nowhere either. Table receivers and string
    /// receivers (`State::index_string`) take the same walk, the register
    /// path's direct answer covers only terminal steps, and the method cache
    /// only skips lookups, so whether a lookup fits under the limit depends
    /// on neither spelling (`t.k`, `t["k"]`, `t:k()`) nor cache history.
    ///
    /// Inlined into its only caller, so that the frame live across the
    /// handler call is that caller's alone: the walk is outlined and has
    /// returned, with the call window already pushed, before the call.
    #[inline(always)]
    fn handle_index_metamethod(
        &mut self,
        receiver: Val,
        key: Val,
        local_cost: &mut u64,
        handler_called: &mut bool,
    ) -> Result<()> {
        let mut units = 0;
        let start = IndexStart::Receiver;
        let result = match self.walk_index_tables(receiver, start, key, local_cost, &mut units) {
            IndexTarget::Done(result) => result,
            IndexTarget::Call => {
                // native-call: handle_index_metamethod->call_unguarded
                self.call_unguarded((ArgCount::Fixed(2), handler_called), RetCount::Fixed(1))
            }
            IndexTarget::Str(handler) => {
                // native-call: handle_index_metamethod->index_string
                self.index_string(handler, key, None, local_cost, handler_called)
            }
        };
        self.metamethod_depth -= units;
        result
    }

    /// The part of an `__index` lookup that calls nothing
    /// (`State::handle_index_metamethod`). Table steps run as a loop, like
    /// the non-function `__call` steps in `State::begin_callable_call`: a
    /// chain of tables adds no native frame per step, each table is read
    /// once, and no code runs between the steps, so the tables passed over
    /// need no rooting. The walk ends at a terminal step, whose result it
    /// pushes; at a function handler, for which it takes the unit, settles
    /// batched cost and pushes the call window `[handler, receiver, key]`
    /// with the table whose metatable named it as the receiver; or at a
    /// string handler, for which it takes the unit. Every unit it takes is
    /// counted in `units`, for the caller to return once the lookup ends.
    #[inline(never)]
    pub(super) fn walk_index_tables(
        &mut self,
        receiver: Val,
        start: IndexStart,
        key: Val,
        local_cost: &mut u64,
        units: &mut u32,
    ) -> IndexTarget {
        let index_key = self.meta_names.get(MetaName::Index);
        let (mut receiver, mut handler, mut receiver_step) = match start {
            IndexStart::Receiver => (receiver, receiver, true),
            IndexStart::Handler(handler) => (receiver, handler, false),
        };
        loop {
            let Some(table) = handler
                .as_object_ptr()
                .and_then(|ptr| self.heap.as_table_ref(ptr))
            else {
                let calls = match handler {
                    Val::Obj(ptr) => self.heap.as_lua_function_ref(ptr).is_some(),
                    Val::RustFn(_) => true,
                    Val::Str(_) => {
                        if let Err(error) = self.take_metamethod_unit() {
                            return IndexTarget::Done(Err(error));
                        }
                        *units += 1;
                        return IndexTarget::Str(handler);
                    }
                    _ => false,
                };
                if !calls {
                    return IndexTarget::Done(Err(self.index_handler_error(handler)));
                }
                if let Err(error) = self.take_metamethod_unit() {
                    return IndexTarget::Done(Err(error));
                }
                *units += 1;
                // Settle batched cost and preflight before any push, so a
                // rejection leaves no temporaries. Cost settlement runs no
                // user code; the receiver is rooted as an argument from here.
                if let Err(error) = Frame::flush_local_cost(self, local_cost)
                    .and_then(|()| self.check_stack_space(3))
                {
                    return IndexTarget::Done(Err(error));
                }
                self.push_unchecked(handler);
                self.push_unchecked(receiver);
                self.push_unchecked(key);
                return IndexTarget::Call;
            };
            let val = table.get(&key);
            let next = match (val, table.get_metatable()) {
                (Val::Nil, Some(mt_ptr)) => self
                    .heap
                    .as_table_ref(mt_ptr)
                    .map_or(Val::Nil, |mt| mt.get(&index_key)),
                _ => Val::Nil,
            };
            if matches!(next, Val::Nil) {
                return IndexTarget::Done(
                    self.check_stack_space(1).map(|()| self.push_unchecked(val)),
                );
            }
            // A table step that continues holds a unit until the lookup
            // ends. The receiver's own lookup is not an `__index` step.
            if receiver_step {
                receiver_step = false;
            } else {
                if let Err(error) = self.take_metamethod_unit() {
                    return IndexTarget::Done(Err(error));
                }
                *units += 1;
            }
            receiver = handler;
            handler = next;
        }
    }

    /// Internal helper for table assignment with __newindex support.
    /// The table should be at stack[idx]. Does not pop anything from the stack.
    pub(super) fn set_table_with_key(
        &mut self,
        idx: usize,
        key: Val,
        val: Val,
        local_cost: &mut u64,
    ) -> Result<()> {
        let table_val = self.stack[idx];
        let obj_ptr = table_val.as_object_ptr();

        // Get existing value and metatable pointer in one heap access
        let (existing, mt_ptr) = match obj_ptr.and_then(|ptr| self.heap.as_table_ref(ptr)) {
            Some(t) => {
                let existing = t.get(&key);
                let mt_ptr = t.get_metatable();
                (existing, mt_ptr)
            }
            None => {
                return Err(self.type_error(super::TypeError::TableIndex(
                    self.stack[idx].typ(&self.heap),
                )));
            }
        };

        if matches!(existing, Val::Nil) {
            // Check for __newindex metamethod
            if let Some(mt_ptr) = mt_ptr {
                let newindex_key = self.meta_names.get(MetaName::NewIndex);
                let newindex_handler = self
                    .heap
                    .as_table_ref(mt_ptr)
                    .map_or(Val::Nil, |mt| mt.get(&newindex_key));

                if !matches!(newindex_handler, Val::Nil) {
                    // native-call: set_table_with_key->handle_newindex_metamethod
                    return self.handle_newindex_metamethod(
                        newindex_handler,
                        idx,
                        key,
                        val,
                        local_cost,
                    );
                }
            }
        }

        // No __newindex or key exists: do normal assignment
        if let Some(ptr) = obj_ptr
            && let Some(t) = self.heap.as_table(ptr)
        {
            t.insert(key, val)?;
        }
        Ok(())
    }

    /// Handle the __newindex metamethod which can be a table or a function.
    fn handle_newindex_metamethod(
        &mut self,
        handler: Val,
        table_idx: usize,
        key: Val,
        val: Val,
        local_cost: &mut u64,
    ) -> Result<()> {
        self.with_metamethod_depth(|state| {
            // native-call: handle_newindex_metamethod->handle_newindex_metamethod_inner
            state.handle_newindex_metamethod_inner(handler, table_idx, key, val, local_cost)
        })
    }

    fn handle_newindex_metamethod_inner(
        &mut self,
        handler: Val,
        table_idx: usize,
        key: Val,
        val: Val,
        local_cost: &mut u64,
    ) -> Result<()> {
        match handler {
            Val::Obj(ptr) => {
                // Check if it's a table or function
                let is_table = self.heap.as_table_ref(ptr).is_some();
                let is_function = self.heap.as_lua_function_ref(ptr).is_some();

                if is_table {
                    // __newindex is a table: set the value in that table instead
                    self.check_stack_space(1)?;
                    self.push_unchecked(Val::Obj(ptr));
                    let new_idx = self.stack.len() - 1;
                    // native-call: handle_newindex_metamethod_inner->set_table_with_key
                    self.set_table_with_key(new_idx, key, val, local_cost)?;
                    self.pop_n(1)?; // Remove the __newindex table
                    Ok(())
                } else if is_function {
                    // __newindex is a function: call it with (table, key, value).
                    // Net-positive by four slots; preflight before any mutation.
                    // Settle batched cost first too, so a budget failure
                    // also leaves no temporaries behind.
                    Frame::flush_local_cost(self, local_cost)?;
                    let table_val = self.stack[table_idx];
                    self.check_stack_space(4)?;
                    self.push_unchecked(Val::Obj(ptr));
                    self.push_unchecked(table_val);
                    self.push_unchecked(key);
                    self.push_unchecked(val);
                    // native-call: handle_newindex_metamethod_inner->call_unguarded:lua
                    self.call_unguarded(ArgCount::Fixed(3), RetCount::Fixed(0))?;
                    Ok(())
                } else {
                    Err(self
                        .type_error(super::TypeError::TableIndex(Val::Obj(ptr).typ(&self.heap))))
                }
            }
            Val::RustFn(f) => {
                // __newindex is a Rust function: call it with (table, key, value).
                // Net-positive by four slots; preflight before any mutation.
                Frame::flush_local_cost(self, local_cost)?;
                let table_val = self.stack[table_idx];
                self.check_stack_space(4)?;
                self.push_unchecked(Val::RustFn(f));
                self.push_unchecked(table_val);
                self.push_unchecked(key);
                self.push_unchecked(val);
                // native-call: handle_newindex_metamethod_inner->call_unguarded:rust
                self.call_unguarded(ArgCount::Fixed(3), RetCount::Fixed(0))?;
                Ok(())
            }
            _ => Err(self.type_error(super::TypeError::TableIndex(handler.typ(&self.heap)))),
        }
    }
}
