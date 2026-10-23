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

impl State {
    /// Run one metamethod step while enforcing the shared recursion limit.
    ///
    /// `__index` and `__newindex` steps count against the limit even when the
    /// handler is a function, so a memoising `__index` that recurses through
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
        let result = operation(self);
        self.metamethod_depth -= 1;
        result
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
        let table_val = self.stack[idx];
        let obj_ptr = table_val.as_object_ptr();

        // Get the value and metatable pointer in one heap access
        let (val, mt_ptr) = match obj_ptr.and_then(|ptr| self.heap.as_table_ref(ptr)) {
            Some(t) => {
                let val = t.get(&key);
                let mt_ptr = t.get_metatable();
                (val, mt_ptr)
            }
            None => {
                return Err(self.type_error(super::TypeError::TableIndex(
                    self.stack[idx].typ(&self.heap),
                )));
            }
        };

        if matches!(val, Val::Nil) {
            // Check for __index metamethod
            if let Some(mt_ptr) = mt_ptr {
                let index_key = self.meta_names.get(MetaName::Index);
                let index_handler = self
                    .heap
                    .as_table_ref(mt_ptr)
                    .map_or(Val::Nil, |mt| mt.get(&index_key));

                if !matches!(index_handler, Val::Nil) {
                    // native-call: get_table_with_key_tracking_call->handle_index_metamethod
                    return self.handle_index_metamethod(
                        index_handler,
                        idx,
                        key,
                        local_cost,
                        handler_called,
                    );
                }
            }
        }

        self.check_stack_space(1)?;
        self.push_unchecked(val);
        Ok(())
    }

    /// Handle the __index metamethod which can be a table or a function.
    fn handle_index_metamethod(
        &mut self,
        handler: Val,
        table_idx: usize,
        key: Val,
        local_cost: &mut u64,
        handler_called: &mut bool,
    ) -> Result<()> {
        self.with_metamethod_depth(|state| {
            // native-call: handle_index_metamethod->handle_index_metamethod_inner
            state.handle_index_metamethod_inner(handler, table_idx, key, local_cost, handler_called)
        })
    }

    fn handle_index_metamethod_inner(
        &mut self,
        handler: Val,
        table_idx: usize,
        key: Val,
        local_cost: &mut u64,
        handler_called: &mut bool,
    ) -> Result<()> {
        match handler {
            Val::Obj(ptr) => {
                // Check if it's a table or function
                let is_table = self.heap.as_table_ref(ptr).is_some();
                let is_function = self.heap.as_lua_function_ref(ptr).is_some();

                if is_table {
                    // __index is a table: look up key in that table
                    self.check_stack_space(1)?;
                    self.push_unchecked(Val::Obj(ptr));
                    let new_idx = self.stack.len() - 1;
                    // The enclosing boundary clears this handler temporary
                    // if recursive lookup fails.
                    // native-call: handle_index_metamethod_inner->get_table_with_key_tracking_call
                    self.get_table_with_key_tracking_call(
                        new_idx,
                        key,
                        local_cost,
                        handler_called,
                    )?;
                    // Stack: [... __index_table, result]
                    // Remove the __index table, keep the result
                    let val = self.pop_val();
                    self.pop_n(1)?;
                    // Net-negative: two values popped above, one pushed back,
                    // so this cannot cross the cap and needs no preflight.
                    self.push_unchecked(val);
                    Ok(())
                } else if is_function {
                    // __index is a function: call it with (table, key).
                    // Net-positive by three slots; preflight so a rejection
                    // leaves the operand stack exactly as it was.
                    // Settle batched cost first too, so a budget failure
                    // also leaves no temporaries behind.
                    Frame::flush_local_cost(self, local_cost)?;
                    // Cost settlement does not call user code. The receiver
                    // remains rooted at this index until the call establishes
                    // a separate callee frame above it.
                    let table_val = self.stack[table_idx];
                    self.check_stack_space(3)?;
                    self.push_unchecked(Val::Obj(ptr));
                    self.push_unchecked(table_val);
                    self.push_unchecked(key);
                    // native-call: handle_index_metamethod_inner->call_unguarded:lua
                    self.call_unguarded((ArgCount::Fixed(2), handler_called), RetCount::Fixed(1))?;
                    Ok(())
                } else {
                    Err(self
                        .type_error(super::TypeError::TableIndex(Val::Obj(ptr).typ(&self.heap))))
                }
            }
            Val::RustFn(f) => {
                // __index is a Rust function: call it with (table, key).
                // Net-positive by three slots; preflight before any mutation.
                Frame::flush_local_cost(self, local_cost)?;
                let table_val = self.stack[table_idx];
                self.check_stack_space(3)?;
                self.push_unchecked(Val::RustFn(f));
                self.push_unchecked(table_val);
                self.push_unchecked(key);
                // native-call: handle_index_metamethod_inner->call_unguarded:rust
                self.call_unguarded((ArgCount::Fixed(2), handler_called), RetCount::Fixed(1))?;
                Ok(())
            }
            // native-call: handle_index_metamethod_inner->index_string
            Val::Str(_) => self.index_string(handler, key, None, local_cost, handler_called),
            _ => Err(self.type_error(super::TypeError::TableIndex(handler.typ(&self.heap)))),
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
