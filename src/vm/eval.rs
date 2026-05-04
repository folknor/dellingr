//! Evaluation and function call logic for the Lua VM.
//!
//! This module contains methods for calling functions, evaluating closures,
//! and managing the call stack.

use std::cmp::Ordering;
use std::rc::Rc;

use super::frame::Frame;
use super::lua_val::Val;
use super::object::{Closure, Upvalue, UpvalueRef};
use super::{
    CallInfo, Chunk, Error, ErrorKind, MAX_CALL_DEPTH, Result, State, TypeError, compiler,
};
use crate::instr::{ArgCount, RetCount};

impl State {
    /// Calls a function.
    ///
    /// To call a function you must use the following protocol: first, the
    /// function to be called is pushed onto the stack; then, the arguments to
    /// the function are pushed in direct order; that is, the first argument is
    /// pushed first. Finally you call `lua_call`; `num_args` is the number of
    /// arguments that you pushed onto the stack. All arguments and the function
    /// value are popped from the stack when the function is called. The
    /// function results are pushed onto the stack when the function returns.
    /// The number of results is adjusted to `num_ret_expected`. The function
    /// results are pushed onto the stack in direct order (the first result is
    /// pushed first), so that after the call the last result is on the top of
    /// the stack.
    pub fn call(&mut self, num_args: ArgCount, num_ret_expected: RetCount) -> Result<()> {
        // Handle vararg call: ArgCount::Dynamic means calculate from vararg_call_bases stack
        let (idx, actual_num_args) = match num_args {
            ArgCount::Dynamic => {
                let base = self
                    .vararg_call_bases
                    .pop()
                    .expect("Call with Dynamic args but no vararg_call_base set");
                let actual = (self.stack.len() - base - 1) as u8;
                (base, actual)
            }
            ArgCount::Fixed(n) => (self.stack.len() - n as usize - 1, n),
        };
        let func_val = self.stack.remove(idx);
        let num_ret_actual = if let Val::RustFn(f) = func_val {
            let old_stack_bottom = self.stack_bottom;
            self.stack_bottom = idx;

            // IMPORTANT: We must restore stack_bottom on ALL exit paths, including errors.
            // Previously, using `f(self)?` here caused a bug: if f() returned Err, the ?
            // operator would propagate the error immediately, skipping the restoration of
            // stack_bottom at the end of this block. This left stack_bottom pointing to
            // `idx` instead of `old_stack_bottom`, corrupting all subsequent stack operations:
            // - get_top() would return wrong values (often 0, thinking the stack is empty)
            // - Stack index calculations would be wrong, causing InvalidStackIndex errors
            // - In release builds, this led to segfaults from out-of-bounds memory access
            //
            // The fix: call f(self) without ?, handle the error explicitly to ensure
            // stack_bottom is restored before propagating the error.
            let result = f(self);
            let num_ret_reported = match result {
                Ok(n) => n,
                Err(e) => {
                    self.stack.truncate(idx);
                    self.stack_bottom = old_stack_bottom;
                    return Err(e);
                }
            };

            let num_ret_actual = self.get_top() as u8;
            match num_ret_reported.cmp(&num_ret_actual) {
                Ordering::Greater => {
                    for _ in num_ret_actual..num_ret_reported {
                        self.push_nil();
                    }
                }
                Ordering::Less => {
                    let slc = &mut self.stack[self.stack_bottom..];
                    slc.rotate_right(num_ret_reported as usize);
                    let new_len =
                        self.stack.len() - num_ret_actual as usize + num_ret_reported as usize;
                    self.stack.truncate(new_len);
                }
                Ordering::Equal => (),
            }
            self.stack_bottom = old_stack_bottom;
            num_ret_reported
        } else if let Some(closure) = func_val.as_lua_function(&self.heap) {
            match self.eval_closure(closure, actual_num_args) {
                Ok(n) => n,
                Err(e) => {
                    self.stack.truncate(idx);
                    return Err(e);
                }
            }
        } else {
            // Check for __call metamethod on tables
            let metatable_ptr = func_val
                .as_object_ptr()
                .and_then(|ptr| self.heap.as_table_ref(ptr))
                .and_then(super::table::Table::get_metatable);

            if let Some(mt_ptr) = metatable_ptr {
                let call_key = self.alloc_string("__call");
                let call_handler = self
                    .heap
                    .as_table_ref(mt_ptr)
                    .map_or(Val::Nil, |mt| mt.get(&call_key));

                if !matches!(call_handler, Val::Nil) {
                    // Insert the table as first argument and call the handler
                    // Stack is currently: [arg1, arg2, ..., argN]
                    // We need: [handler, table, arg1, arg2, ..., argN]
                    self.stack.insert(idx, func_val);
                    self.stack.insert(idx, call_handler);
                    // Now call with actual_num_args + 1 (table is first arg)
                    return self.call(ArgCount::Fixed(actual_num_args + 1), num_ret_expected);
                }
            }
            self.stack.truncate(idx);
            return Err(self.type_error(TypeError::FunctionCall(func_val.typ(&self.heap))));
        };
        // RetCount::All means "return all" - don't adjust the stack
        if let RetCount::Fixed(expected) = num_ret_expected {
            self.balance_stack(expected as usize, num_ret_actual as usize);
        }
        Ok(())
    }

    /// Loads a string as a Lua chunk. This function uses `load` to load the
    /// chunk in the string `s`.
    pub fn load_string(&mut self, s: impl AsRef<str>) -> Result<()> {
        self.load_string_named(s, None)
    }

    /// Loads a string as a Lua chunk with an optional source name.
    /// The source name is used in error messages and stack traces.
    /// Use a filename for files, or something like "[fleet:123]" for dynamically loaded code.
    pub fn load_string_named(
        &mut self,
        s: impl AsRef<str>,
        source_name: Option<String>,
    ) -> Result<()> {
        self.current_source = source_name.clone();
        let c = compiler::parse_str_named(s, source_name)?;
        self.push_chunk(c);
        Ok(())
    }

    /// Pops `n` values from the stack, concatenates them, and pushes the
    /// result. If `n` is 1, the result is the single value on the stack (that
    /// is, the function does nothing); if `n` is 0, the result is the empty
    /// string.
    pub fn concat(&mut self, n: usize) -> Result<()> {
        if n < 2 {
            return Err(Error::without_location(ErrorKind::ArgError(
                crate::error::ArgError {
                    arg_number: 1,
                    func_name: Some("concat".to_string()),
                    expected: None,
                    received: None,
                },
            )));
        }
        self.concat_helper(n)
    }

    pub(super) fn concat_helper(&mut self, n: usize) -> Result<()> {
        let mut buffer = Vec::new();
        let idx = self.stack.len() - n;
        // Collect values first so we can access heap for strings
        let values: Vec<Val> = self.stack.drain(idx..).collect();

        for val in &values {
            if let Some(s) = val.as_string(&self.heap) {
                buffer.extend_from_slice(s);
            } else if let Some(num) = val.as_num() {
                // Auto-convert numbers to strings (standard Lua behavior)
                // Format integers without decimal point, floats with
                if num.fract() == 0.0 && num.abs() < 1e15 {
                    buffer.extend_from_slice(format!("{}", num as i64).as_bytes());
                } else {
                    buffer.extend_from_slice(format!("{num}").as_bytes());
                }
            } else {
                return Err(self.type_error(TypeError::Concat(val.typ_simple())));
            }
        }

        let val = self.alloc_string(&buffer);
        self.stack.push(val);
        Ok(())
    }

    pub(super) fn eval_closure(&mut self, closure: Closure, num_args: u8) -> Result<u8> {
        // Check call depth limit
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(Error::without_location(ErrorKind::CallDepthExceeded {
                depth: self.call_depth,
            }));
        }
        self.call_depth += 1;

        let result = self.eval_closure_inner(closure, num_args);

        self.call_depth -= 1;
        result
    }

    fn eval_closure_inner(&mut self, closure: Closure, num_args: u8) -> Result<u8> {
        let old_stack_bottom = self.stack_bottom;
        self.stack_bottom = self.stack.len() - num_args as usize;

        let num_params = closure.chunk.num_params;
        let num_locals = closure.chunk.num_locals;
        let is_vararg = closure.chunk.is_vararg;

        // Push call info for stack traces
        self.call_stack.push(CallInfo {
            chunk: Rc::clone(&closure.chunk),
            ip: 0,
        });

        // Collect varargs if this is a vararg function
        let varargs = if is_vararg && num_args > num_params {
            let num_varargs = (num_args - num_params) as usize;
            let vararg_start = self.stack.len() - num_varargs;
            self.stack.drain(vararg_start..).collect()
        } else {
            Vec::new()
        };

        // Check stack space for parameters and locals
        let extra_params = if num_args < num_params {
            (num_params - num_args) as usize
        } else {
            0
        };
        if let Err(e) = self.check_stack_space(extra_params + num_locals as usize) {
            // Must restore stack_bottom before returning error (see comment in RustFn handling)
            self.stack_bottom = old_stack_bottom;
            self.call_stack.pop();
            return Err(e);
        }

        match num_args.cmp(&num_params) {
            Ordering::Less => {
                for _ in num_args..num_params {
                    self.push_nil();
                }
            }
            Ordering::Greater => {
                if !is_vararg {
                    self.pop((num_args - num_params) as isize);
                }
                // If is_vararg, we already collected the extra args above
            }
            Ordering::Equal => (),
        }

        for _ in 0..num_locals {
            self.push_nil();
        }

        let mut frame = self.initialize_frame(closure, varargs);
        let ret_count = match frame.eval(self) {
            Ok(count) => count,
            Err(e) => {
                // Only attach stack trace if error doesn't already have one
                // (inner function calls may have already attached the trace)
                let e = if e.stack_trace.is_empty() {
                    let trace = self.build_stack_trace(&frame);
                    let e = e.with_stack_trace(trace);
                    // Notify host callbacks of the error
                    self.host_error(&e);
                    e
                } else {
                    e
                };
                // Must restore stack_bottom before returning error (see comment in RustFn handling)
                self.close_upvalues(self.stack_bottom);
                self.stack.truncate(self.stack_bottom);
                self.stack_bottom = old_stack_bottom;
                self.call_stack.pop();
                return Err(e);
            }
        };

        // Handle RetCount::All which means "return all values on stack"
        let actual_num_returned = match ret_count {
            RetCount::All => {
                // Calculate how many values are above the frame's locals
                let frame_base = self.stack_bottom + num_params as usize + num_locals as usize;
                (self.stack.len() - frame_base) as u8
            }
            RetCount::Fixed(n) => n,
        };

        // Save return values from the top of the stack
        let ret_start = self.stack.len() - actual_num_returned as usize;
        let ret_vals: Vec<Val> = self.stack.drain(ret_start..).collect();

        // Close any open upvalues in this frame before clearing the stack
        self.close_upvalues(self.stack_bottom);

        // Clear the frame's stack space
        self.stack.truncate(self.stack_bottom);
        self.stack_bottom = old_stack_bottom;

        // Push return values back onto the stack
        self.stack.extend(ret_vals);

        // Pop call info
        self.call_stack.pop();

        Ok(actual_num_returned)
    }

    pub(super) fn initialize_frame(&mut self, closure: Closure, varargs: Vec<Val>) -> Frame {
        let string_literal_start = self.string_literals.len();
        for s in &closure.chunk.string_literals {
            // Check if GC is needed before allocating
            if self.heap.is_full() {
                self.gc_collect();
            }
            let string_ptr = self.heap.alloc_string(s);
            self.string_literals.push(Val::Str(string_ptr));
        }
        Frame::new(
            closure.chunk,
            closure.upvalues,
            varargs,
            string_literal_start,
            self.stack_bottom,
        )
    }

    pub(super) fn push_chunk(&mut self, chunk: Chunk) {
        self.push_closure(chunk, Vec::new());
    }

    pub(super) fn push_closure(&mut self, chunk: Chunk, upvalues: Vec<UpvalueRef>) {
        // Check if GC is needed before allocating
        if self.heap.is_full() {
            self.gc_collect();
        }
        let obj = self.heap.alloc_lua_fn(chunk, upvalues);
        self.stack.push(Val::Obj(obj));
    }

    /// Find an existing open upvalue for the given stack index, or create a new one.
    pub(super) fn find_or_create_upvalue(&mut self, stack_idx: usize) -> UpvalueRef {
        // Check if we already have an open upvalue for this stack slot
        for (idx, uv_ref) in &self.open_upvalues {
            if *idx == stack_idx {
                return *uv_ref;
            }
        }
        // Create a new open upvalue in the pool
        let uv_ref = self.upvalue_pool.alloc(Upvalue::Open(stack_idx));
        // Insert in order (sorted by stack index ascending, so we can pop from end in O(1))
        let pos = self
            .open_upvalues
            .iter()
            .position(|(idx, _)| *idx > stack_idx)
            .unwrap_or(self.open_upvalues.len());
        self.open_upvalues.insert(pos, (stack_idx, uv_ref));
        uv_ref
    }

    /// Close all open upvalues at or above the given stack level.
    /// This is called when a function returns to capture the values from the stack
    /// before they are popped.
    pub(crate) fn close_upvalues(&mut self, level: usize) {
        // Upvalues are sorted ascending by stack index, so highest indices are at the end.
        // We close highest first (they go out of scope first), popping from end in O(1).
        while let Some(&(idx, _)) = self.open_upvalues.last() {
            if idx < level {
                break;
            }
            let (_, uv_ref) = self
                .open_upvalues
                .pop()
                .expect("open upvalue existed after checking last element");
            let val = self.stack[idx];
            *self.upvalue_pool.get_mut(uv_ref) = Upvalue::Closed(val);
        }
    }

    /// Helper for tests: evaluate a chunk with no upvalues.
    #[cfg(test)]
    pub(super) fn eval_chunk(&mut self, chunk: Chunk, num_args: u8) -> Result<u8> {
        let closure = Closure {
            chunk: std::rc::Rc::new(chunk),
            upvalues: Vec::new(),
        };
        self.eval_closure(closure, num_args)
    }
}
