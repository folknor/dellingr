//! Evaluation and function call logic for the Lua VM.
//!
//! This module contains methods for calling functions, evaluating closures,
//! and managing the call stack.

use std::cmp::Ordering;
use std::sync::Arc;

use super::frame::Frame;
use super::lua_val::Val;
use super::object::{Closure, Upvalue, UpvalueRef};
use super::{
    Bytecode, CallInfo, Error, ErrorKind, MAX_CALL_DEPTH, Result, State, TypeError, compiler,
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
    ///
    /// # Errors
    ///
    /// A fixed call with insufficient stack values returns `InvalidStackIndex`.
    /// Dynamic calls are bytecode-internal and return an error if used by a
    /// host. Argument and result counts are limited to 255.
    pub fn call(&mut self, num_args: ArgCount, num_ret_expected: RetCount) -> Result<()> {
        // Handle vararg call: ArgCount::Dynamic means calculate from vararg_call_bases stack
        let (idx, actual_num_args) = match num_args {
            ArgCount::Dynamic => {
                let base = self.vararg_call_bases.pop().ok_or_else(|| {
                    self.error(ErrorKind::InternalError(
                        "call with Dynamic args but no vararg_call_base set".into(),
                    ))
                })?;
                let actual_len = self.stack.len().checked_sub(base + 1).ok_or_else(|| {
                    self.error(ErrorKind::InternalError(
                        "dynamic call base is outside the active stack".into(),
                    ))
                })?;
                let actual = u8::try_from(actual_len).map_err(|_| {
                    self.error(ErrorKind::RuntimeError(
                        "too many arguments (limit 255)".into(),
                    ))
                })?;
                (base, actual)
            }
            ArgCount::Fixed(n) => {
                let required = usize::from(n) + 1;
                let offset = self.get_top().checked_sub(required).ok_or_else(|| {
                    self.error(ErrorKind::InvalidStackIndex {
                        index: -isize::from(n) - 1,
                    })
                })?;
                (self.stack_bottom + offset, n)
            }
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
                    // Notify the host of a Rust-function failure it reached
                    // directly (no Lua frame on the stack). When call_depth > 0
                    // an enclosing Lua frame will fire on_error as the error
                    // unwinds; a non-empty stack trace means it already did. This
                    // guard keeps on_error firing exactly once per error (L6).
                    if self.call_depth == 0 && e.stack_trace.is_empty() {
                        self.host_error(&e);
                    }
                    return Err(e);
                }
            };

            let num_ret_actual = self.get_top();
            let reported = usize::from(num_ret_reported);
            match reported.cmp(&num_ret_actual) {
                Ordering::Greater => {
                    for _ in num_ret_actual..reported {
                        self.push_nil();
                    }
                }
                Ordering::Less => {
                    let slc = &mut self.stack[self.stack_bottom..];
                    slc.rotate_right(reported);
                    let new_len = self.stack.len() - num_ret_actual + reported;
                    self.stack.truncate(new_len);
                }
                Ordering::Equal => (),
            }
            self.stack_bottom = old_stack_bottom;
            num_ret_reported
        } else if let Some(closure) = func_val.as_lua_function(&self.heap) {
            // Deliberately a manual push/pop rather than `with_rooted_value`.
            // This is the recursive Lua-call path, and the helper's closure plus
            // catch_unwind landing pad add enough per-level stack-frame bloat to
            // abort `call_depth_exceeded_error`, which recurses to
            // MAX_CALL_DEPTH = 1000 (the same constraint AGENTS.md records for
            // #[hotpath::measure] on this path). A Rust panic unwinding through
            // here therefore leaks this root - but it also leaves call_stack,
            // string_literals, stack_bottom and the vararg bases inconsistent,
            // none of which are unwound either. A State that has been unwound
            // through must be discarded, not reused.
            self.transient_roots.values.push(func_val);
            let result = self.eval_closure(closure, actual_num_args);
            let popped = self
                .transient_roots
                .values
                .pop()
                .expect("active call root missing after Lua call");
            debug_assert!(popped == func_val);
            match result {
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
                let call_handler = self.with_rooted_value(func_val, |state| {
                    let call_key = state
                        .alloc_string("__call")
                        .expect("a fixed metamethod name is far below MAX_STRING_BYTES");
                    state
                        .heap
                        .as_table_ref(mt_ptr)
                        .map_or(Val::Nil, |mt| mt.get(&call_key))
                });

                if !matches!(call_handler, Val::Nil) {
                    // Insert the table as first argument and call the handler
                    // Stack is currently: [arg1, arg2, ..., argN]
                    // We need: [handler, table, arg1, arg2, ..., argN]
                    let combined = match actual_num_args.checked_add(1) {
                        Some(c) => c,
                        None => {
                            // Match the other call error paths: clear the frame
                            // (the callable was already removed above) so a direct
                            // host call does not leave the arguments visible.
                            self.stack.truncate(idx);
                            return Err(self.error(ErrorKind::RuntimeError(
                                "too many arguments (limit 255)".into(),
                            )));
                        }
                    };
                    self.stack.insert(idx, func_val);
                    self.stack.insert(idx, call_handler);
                    // Now call with actual_num_args + 1 (table is first arg)
                    return self.call(ArgCount::Fixed(combined), num_ret_expected);
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
    #[hotpath::measure]
    pub fn load_string(&mut self, s: impl AsRef<str>) -> Result<()> {
        self.load_string_named(s, None)
    }

    /// Loads a string as a Lua chunk with an optional source name.
    /// The source name is used in error messages and stack traces.
    /// Use a filename for files, or something like `"[fleet:123]"` for dynamically loaded code.
    #[hotpath::measure]
    pub fn load_string_named(
        &mut self,
        s: impl AsRef<str>,
        source_name: Option<String>,
    ) -> Result<()> {
        self.current_source = source_name.clone();
        let bytecode = compiler::parse_str_named(s, source_name)?;
        self.push_chunk(Arc::new(bytecode));
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

    #[hotpath::measure]
    pub(super) fn concat_helper(&mut self, n: usize) -> Result<()> {
        let idx = self.stack.len() - n;

        // First pass: type-check and compute the EXACT total byte length.
        //
        // Numbers are rendered here rather than estimated. A fixed estimate is
        // wrong in both directions: 32 bytes over-counts `1` and would reject a
        // legal result - which would also change what the script costs, since
        // the next costed operation never runs - while under-counting the likes
        // of `1e308`, which Rust renders as 309 bytes, would let the buffer grow
        // past the cap unchecked.
        let mut total_len = 0;
        let mut rendered_numbers = Vec::new();
        for val in &self.stack[idx..] {
            if let Some(s) = val.as_string(&self.heap) {
                total_len = super::checked_string_growth(total_len, s.len())?;
            } else if let Some(num) = val.as_num() {
                // Auto-convert numbers to strings (standard Lua behavior).
                // Format integers without decimal point, floats with.
                let rendered = if num.fract() == 0.0 && num.abs() < 1e15 {
                    format!("{}", num as i64)
                } else {
                    format!("{num}")
                };
                total_len = super::checked_string_growth(total_len, rendered.len())?;
                rendered_numbers.push(rendered);
            } else {
                return Err(self.type_error(TypeError::Concat(val.typ(&self.heap))));
            }
        }

        let mut buffer = Vec::with_capacity(total_len);
        let mut rendered = rendered_numbers.iter();
        for val in &self.stack[idx..] {
            if let Some(s) = val.as_string(&self.heap) {
                buffer.extend_from_slice(s);
            } else if val.as_num().is_some() {
                let text = rendered
                    .next()
                    .expect("one rendering was produced per numeric operand above");
                buffer.extend_from_slice(text.as_bytes());
            }
        }

        // Intern before truncating, so a rejected concat leaves the operands on
        // the stack rather than consuming them.
        let val = self.alloc_string(&buffer)?;
        self.stack.truncate(idx);
        self.stack.push(val);
        Ok(())
    }

    pub(super) fn eval_closure(&mut self, closure: Closure, num_args: u8) -> Result<u8> {
        for string in &closure.bytecode.string_literals {
            super::check_string_size(string.len())?;
        }
        // Check call depth limit
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(Error::without_location(ErrorKind::CallDepthExceeded {
                depth: self.call_depth,
            }));
        }
        self.call_depth += 1;

        // Watermark the dynamic-call-base stacks on entry. A well-formed frame
        // pushes and consumes these in balance, but an error between an
        // OP_MARK_CALL_BASE and its Dynamic OP_CALL (or between NewTableTracked
        // and its SetList) leaves a stale base behind. The per-frame error
        // cleanup in eval_closure_inner does not touch these stacks, so truncate
        // them here on any error so the State stays quiescent (reusable /
        // snapshot-saveable) after a killed callback (L8).
        let vararg_base_watermark = self.vararg_call_bases.len();
        let table_base_watermark = self.table_constructor_bases.len();

        let result = self.eval_closure_inner(closure, num_args);

        if result.is_err() {
            self.vararg_call_bases.truncate(vararg_base_watermark);
            self.table_constructor_bases.truncate(table_base_watermark);
        }

        self.call_depth -= 1;
        result
    }

    fn eval_closure_inner(&mut self, closure: Closure, num_args: u8) -> Result<u8> {
        let old_stack_bottom = self.stack_bottom;
        self.stack_bottom = self.stack.len() - num_args as usize;

        let num_params = closure.bytecode.num_params;
        let is_vararg = closure.bytecode.is_vararg;

        // Push call info for stack traces
        self.call_stack.push(CallInfo {
            bytecode: Arc::clone(&closure.bytecode),
            ip: 0,
        });

        // Collect and root varargs only for a frame that has extra arguments.
        // Calls without extra varargs intentionally avoid all root bookkeeping.
        if is_vararg && num_args > num_params {
            let num_varargs = (num_args - num_params) as usize;
            let vararg_start = self.stack.len() - num_varargs;
            let varargs: Vec<Val> = self.stack.drain(vararg_start..).collect();
            // Root from the slice before moving `varargs` into the frame:
            // with_rooted_values copies into transient_roots anyway, so cloning
            // the Vec first would allocate twice per vararg call for nothing.
            let watermark = self.transient_roots.values.len();
            self.transient_roots.values.extend_from_slice(&varargs);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.eval_closure_frame(closure, old_stack_bottom, num_args, varargs)
            }));
            self.transient_roots.values.truncate(watermark);
            return match result {
                Ok(result) => result,
                Err(payload) => std::panic::resume_unwind(payload),
            };
        }

        self.eval_closure_frame(closure, old_stack_bottom, num_args, Vec::new())
    }

    /// Fill in a runtime error's source line from the frame it surfaced in.
    ///
    /// Every error-return path out of a Lua frame must go through this, so that
    /// the line rendered alongside the traceback's source always comes from the
    /// same frame. An error that already carries a location keeps it - that is
    /// how parser errors survive with their own line and column.
    fn locate_in_frame(error: Error, frame: &Frame) -> Error {
        if error.line_num == 0 {
            let column = error.column;
            error.with_location(frame.current_line() as usize, column)
        } else {
            error
        }
    }

    fn eval_closure_frame(
        &mut self,
        closure: Closure,
        old_stack_bottom: usize,
        num_args: u8,
        varargs: Vec<Val>,
    ) -> Result<u8> {
        let num_params = closure.bytecode.num_params;
        let num_locals = closure.bytecode.num_locals;
        let is_vararg = closure.bytecode.is_vararg;
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
                    self.pop((num_args - num_params) as isize)?;
                }
                // If is_vararg, we already collected the extra args above
            }
            Ordering::Equal => (),
        }

        for _ in 0..num_locals {
            self.push_nil();
        }

        let mut frame = self.initialize_frame(closure, varargs);
        let string_literal_start = frame.string_literal_start();
        let ret_count = match frame.eval(self) {
            Ok(count) => count,
            Err(e) => {
                // Only attach stack trace if error doesn't already have one
                // (inner function calls may have already attached the trace)
                let e = if e.stack_trace.is_empty() {
                    let e = Self::locate_in_frame(e, &frame);
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
                self.string_literals.truncate(string_literal_start);
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
                let count = self.stack.len() - frame_base;
                match u8::try_from(count) {
                    Ok(n) => n,
                    Err(_) => {
                        let e = self.error(ErrorKind::RuntimeError(
                            "too many results (limit 255)".into(),
                        ));
                        let e = Self::locate_in_frame(e, &frame);
                        let trace = self.build_stack_trace(&frame);
                        let e = e.with_stack_trace(trace);
                        self.host_error(&e);
                        self.close_upvalues(self.stack_bottom);
                        self.stack.truncate(self.stack_bottom);
                        self.string_literals.truncate(string_literal_start);
                        self.stack_bottom = old_stack_bottom;
                        self.call_stack.pop();
                        return Err(e);
                    }
                }
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
        self.string_literals.truncate(string_literal_start);
        self.stack_bottom = old_stack_bottom;

        // Push return values back onto the stack
        self.stack.extend(ret_vals);

        // Pop call info
        self.call_stack.pop();

        Ok(actual_num_returned)
    }

    #[hotpath::measure]
    pub(super) fn initialize_frame(&mut self, closure: Closure, varargs: Vec<Val>) -> Frame {
        let string_literal_start = self.string_literals.len();
        for s in &closure.bytecode.string_literals {
            // Check if GC is needed before allocating
            if self.heap.is_full() {
                self.gc_collect();
            }
            let string_ptr = self.heap.alloc_string(s);
            self.string_literals.push(Val::Str(string_ptr));
        }
        Frame::new(
            closure.bytecode,
            closure.caches,
            closure.upvalues,
            varargs,
            string_literal_start,
            self.stack_bottom,
        )
    }

    pub(crate) fn push_chunk(&mut self, bytecode: Arc<Bytecode>) {
        self.push_closure(bytecode, Vec::new());
    }

    #[hotpath::measure]
    pub(super) fn push_closure(&mut self, bytecode: Arc<Bytecode>, upvalues: Vec<UpvalueRef>) {
        // Check if GC is needed before allocating
        if self.heap.is_full() {
            self.gc_collect();
        }
        let obj = self.heap.alloc_lua_fn(bytecode, upvalues);
        self.stack.push(Val::Obj(obj));
    }

    /// Find an existing open upvalue for the given stack index, or create a new one.
    #[hotpath::measure]
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
    #[hotpath::measure]
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

    /// Helper for tests: evaluate a bytecode with no upvalues.
    #[cfg(test)]
    pub(super) fn eval_chunk(&mut self, bytecode: Bytecode, num_args: u8) -> Result<u8> {
        let bytecode = Arc::new(bytecode);
        let caches = Arc::new(super::RuntimeCaches::new(&bytecode));
        let closure = Closure {
            bytecode,
            caches,
            upvalues: Vec::new(),
        };
        self.eval_closure(closure, num_args)
    }
}
