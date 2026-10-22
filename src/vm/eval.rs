//! Evaluation and function call logic for the Lua VM.
//!
//! This module contains methods for calling functions, evaluating closures,
//! and managing the call stack.

use std::cmp::Ordering;
use std::sync::Arc;

use super::frame::Frame;
use super::lua_val::{RustFunc, Val};
use super::object::{Upvalue, UpvalueRef};
use super::{Bytecode, CallInfo, Error, ErrorKind, MAX_CALL_DEPTH, Result, State, TypeError};
use crate::instr::{ArgCount, RetCount};
use crate::limits::{MAX_METAMETHOD_DEPTH, MAX_NATIVE_CALL_DEPTH};

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
    /// host. Argument and result counts are bounded by `limits::MAX_ARGUMENTS` and
    /// `limits::MAX_RESULTS`. Reporting state follows the Error value: returning
    /// an already reported Error from a later callback does not report it again.
    ///
    /// A poisoned State (see [`State::is_poisoned`]) is refused with
    /// `StatePoisoned`. A panic out of the callee poisons the State and
    /// propagates; a callback that catches a panic and returns normally gets
    /// its execution abandoned, and this call returns `StatePoisoned`.
    pub fn call(&mut self, num_args: ArgCount, num_ret_expected: RetCount) -> Result<()> {
        self.guarded(|state| state.call_unguarded(num_args, num_ret_expected))
    }

    /// The body of [`State::call`] without the poison boundary. Every
    /// VM-internal caller (metamethod dispatch, the generic-for iterator
    /// call, the standard library) uses this one; see `State::guarded` for
    /// why the boundary must only face host code.
    ///
    /// This is the one way Rust code calls a function, and therefore the one
    /// place native recursion can grow: each active call made through here
    /// counts against `MAX_NATIVE_CALL_DEPTH`. Lua-to-Lua calls never come
    /// through here; the dispatch loop runs them in place.
    pub(crate) fn call_unguarded(
        &mut self,
        num_args: ArgCount,
        num_ret_expected: RetCount,
    ) -> Result<()> {
        // ArgCount::Dynamic is resolved by the executing Lua Frame against
        // its own pending base. Hosts never own such a base, so the public
        // entry point rejects Dynamic before touching any state.
        //
        // This and the InvalidStackIndex preflight below are returned
        // without notifying on_error: they are host misuse detected before
        // anything executes, which the on_error doc in host.rs excludes by
        // contract. Every failure after dispatch reaches an error-reporting
        // boundary.
        let num_args = match num_args {
            ArgCount::Fixed(num_args) => num_args,
            ArgCount::Dynamic => {
                return Err(Error::without_location(
                    ErrorKind::DynamicArgumentsUnsupported {
                        api: crate::error::HostCallApi::Call,
                    },
                ));
            }
        };
        let idx = self.call_base(num_args)?;
        let old_stack_bottom = self.stack_bottom;
        if self.native_call_depth >= MAX_NATIVE_CALL_DEPTH {
            // Rejected before dispatch, but after the callee was named, so it
            // fails like a call that started: the callee and its arguments
            // are cleared. It can only happen inside a callback, whose
            // enclosing boundary reports it.
            self.truncate_call_frame(idx, old_stack_bottom);
            return Err(self.error(ErrorKind::NativeCallDepthExceeded {
                depth: self.native_call_depth,
                limit: MAX_NATIVE_CALL_DEPTH,
            }));
        }
        self.native_call_depth += 1;
        let result = self.call_from_rust(idx, num_args, num_ret_expected, old_stack_bottom);
        self.native_call_depth -= 1;
        result
    }

    /// Stack index of the callee of a call with `num_args` arguments on top
    /// of the current frame.
    fn call_base(&self, num_args: u8) -> Result<usize> {
        let required = usize::from(num_args) + 1;
        let offset = self.get_top().checked_sub(required).ok_or_else(|| {
            self.error(ErrorKind::InvalidStackIndex {
                index: -isize::from(num_args) - 1,
            })
        })?;
        Ok(self.stack_bottom + offset)
    }

    /// A call made from Rust: resolve the callee, run it to completion on
    /// this native stack (a Lua callee gets its own dispatch-loop
    /// invocation), and adjust its results.
    ///
    /// The callee owns only slots at or above its new stack_bottom. Public
    /// stack operations reject indices and truncations below that boundary;
    /// nested Lua locals and temporaries are also placed above it. Returning
    /// restores the caller's slot positions even if pushes reallocate the
    /// buffer. Captured locals can still be written through explicit upvalues.
    fn call_from_rust(
        &mut self,
        idx: usize,
        num_args: u8,
        num_ret_expected: RetCount,
        old_stack_bottom: usize,
    ) -> Result<()> {
        let mut charges = 0;
        let num_ret_actual =
            match self.begin_call(idx, num_args, old_stack_bottom, &mut charges, false)? {
                Some(n) => n,
                None => match self.run_invocation(old_stack_bottom) {
                    Ok(n) => n,
                    Err(e) => return Err(self.fail_call(e, idx, old_stack_bottom, &mut charges)),
                },
            };
        self.finish_call(
            idx,
            old_stack_bottom,
            num_ret_expected,
            num_ret_actual,
            charges,
        )
    }

    /// A call made by the dispatch loop's `OP_CALL`. A Rust callee runs to
    /// completion here and `false` is returned. A Lua callee is admitted
    /// (activation record pushed, frame slots laid out, any `__call` chain
    /// charges handed to the record) and `true` is returned: the loop exits
    /// to its driver, which pushes the callee's frame and runs it without
    /// native recursion.
    ///
    /// Out of line on purpose: inlined, the call machinery grew the dispatch
    /// loop's code and register pressure enough to slow every opcode.
    #[inline(never)]
    pub(super) fn call_from_lua(
        &mut self,
        num_args: u8,
        num_ret_expected: RetCount,
        method: bool,
    ) -> Result<bool> {
        let idx = self.call_base(num_args)?;
        let old_stack_bottom = self.stack_bottom;
        let mut charges = 0;
        match self.begin_call(idx, num_args, old_stack_bottom, &mut charges, method)? {
            Some(n) => {
                self.finish_call(idx, old_stack_bottom, num_ret_expected, n, charges)?;
                Ok(false)
            }
            None => {
                self.call_stack
                    .last_mut()
                    .expect("an admitted Lua call has an activation record")
                    .chain_charges = charges;
                Ok(true)
            }
        }
    }

    /// Adjust a completed call's results to the expected count, then release
    /// the `__call` chain charges its resolution held.
    fn finish_call(
        &mut self,
        idx: usize,
        old_stack_bottom: usize,
        num_ret_expected: RetCount,
        num_ret_actual: u8,
        mut charges: u32,
    ) -> Result<()> {
        // RetCount::All means "return all" - don't adjust the stack
        if let RetCount::Fixed(expected) = num_ret_expected {
            if expected < num_ret_actual {
                // Surplus results, the common case of a call statement:
                // drop them without a call.
                let surplus = usize::from(num_ret_actual - expected);
                self.stack.truncate(self.stack.len() - surplus);
            } else if expected > num_ret_actual {
                // Padding the results up to `expected` can hit the cap.
                // Clear the call's frame on failure like every other error
                // path here, so the caller is not left holding a partial
                // result set.
                if let Err(e) = self.balance_stack(expected as usize, num_ret_actual as usize) {
                    return Err(self.fail_call(e, idx, old_stack_bottom, &mut charges));
                }
            }
        }
        self.release_chain_charges(charges);
        Ok(())
    }

    /// Resolve the callable at `idx` (with `num_args` arguments above it)
    /// and start the call. A Rust callee runs to completion and its result
    /// count is returned; a Lua callee is admitted (activation record pushed,
    /// frame slots laid out) but not run, and `None` is returned.
    ///
    /// A callable object is resolved iteratively through `__call`. A
    /// function-valued handler is free; each handler that is itself not a
    /// function adds one unit to `metamethod_depth` and `call_chain_depth`,
    /// recorded in `charges`, and those units stay held until the whole call
    /// completes: the caller owns their release on success. Only that case
    /// counts against the metamethod depth: a Lua function handler is bounded
    /// by call_depth like any other Lua call, and guarding it too would cap
    /// legitimate recursion through callable objects at MAX_METAMETHOD_DEPTH.
    ///
    /// On error the charges are released, the call's slots are cleared, the
    /// caller's stack_bottom is restored, and the error has passed through
    /// `report_call_error`.
    #[inline(always)]
    fn begin_call(
        &mut self,
        idx: usize,
        num_args: u8,
        old_stack_bottom: usize,
        charges: &mut u32,
        method: bool,
    ) -> Result<Option<u8>> {
        // The two function cases inline into the call sites; only a
        // callable object pays for the resolution loop's setup.
        let func_val = self.stack[idx];
        if let Val::RustFn(f) = func_val {
            self.stack.remove(idx);
            return match self.invoke_rust_fn(f, idx, method) {
                Ok(n) => Ok(Some(n)),
                Err(e) => Err(self.fail_call(e, idx, old_stack_bottom, charges)),
            };
        }
        if let Some(bytecode) = self.lua_function_bytecode(func_val) {
            return match self.admit_lua_call(bytecode, num_args, old_stack_bottom) {
                Ok(()) => Ok(None),
                Err(e) => Err(self.fail_call(e, idx, old_stack_bottom, charges)),
            };
        }
        self.begin_callable_call(idx, num_args, old_stack_bottom, charges, method)
    }

    /// `begin_call` for a callee that is not a function: the general
    /// resolution loop, which also handles the function cases it resolves to.
    #[inline(never)]
    fn begin_callable_call(
        &mut self,
        idx: usize,
        num_args: u8,
        old_stack_bottom: usize,
        charges: &mut u32,
        method: bool,
    ) -> Result<Option<u8>> {
        let mut num_args = num_args;
        loop {
            let func_val = self.stack[idx];
            if let Val::RustFn(f) = func_val {
                // The Rust callback ABI: the callee's frame starts at the
                // first argument, with the function value gone.
                self.stack.remove(idx);
                return match self.invoke_rust_fn(f, idx, method) {
                    Ok(n) => Ok(Some(n)),
                    Err(e) => Err(self.fail_call(e, idx, old_stack_bottom, charges)),
                };
            }
            if let Some(bytecode) = self.lua_function_bytecode(func_val) {
                // A Lua callee keeps its function slot: the value stays
                // rooted there below the callee's frame, its frame is built
                // from it (`State::admitted_frame`), and the results land in
                // it on return.
                return match self.admit_lua_call(bytecode, num_args, old_stack_bottom) {
                    Ok(()) => Ok(None),
                    Err(e) => Err(self.fail_call(e, idx, old_stack_bottom, charges)),
                };
            }
            // Check for a __call metamethod: a table's own metatable, or the
            // metatable all strings share (reference Lua makes `("x")(...)`
            // callable once `getmetatable("").__call` is set). This branch is
            // already off the function fast paths above.
            let metatable_ptr = match func_val {
                Val::Obj(ptr) => self
                    .heap
                    .as_table_ref(ptr)
                    .and_then(super::table::Table::get_metatable),
                Val::Str(_) => self.string_metatable,
                _ => None,
            };
            let call_handler = metatable_ptr.map_or(Val::Nil, |mt_ptr| {
                let call_key = self.meta_names.get(super::MetaName::Call);
                self.heap
                    .as_table_ref(mt_ptr)
                    .map_or(Val::Nil, |mt| mt.get(&call_key))
            });
            if matches!(call_handler, Val::Nil) {
                // Resolving a callable is execution, even when it has no
                // handler, so this reaches the reporting boundary too.
                let error = self.type_error(TypeError::FunctionCall(func_val.typ(&self.heap)));
                return Err(self.fail_call(error, idx, old_stack_bottom, charges));
            }
            // Insert the receiver as first argument and call the handler.
            // Stack is currently: [callable, arg1, arg2, ..., argN]
            // We need: [handler, receiver, arg1, arg2, ..., argN]
            let Some(combined) = num_args.checked_add(1) else {
                let error = self.error(ErrorKind::TooManyArguments {
                    limit: crate::limits::MAX_ARGUMENTS,
                });
                return Err(self.fail_call(error, idx, old_stack_bottom, charges));
            };
            // The handler takes the callable's slot and the receiver is
            // inserted after it: exactly one slot more than the call began
            // with.
            if let Err(e) = self.check_stack_space(1) {
                return Err(self.fail_call(e, idx, old_stack_bottom, charges));
            }
            self.stack[idx] = call_handler;
            self.stack.insert(idx + 1, func_val);
            num_args = combined;
            let handler_is_function = matches!(call_handler, Val::RustFn(_))
                || call_handler.as_lua_function(&self.heap).is_some();
            if !handler_is_function {
                if self.metamethod_depth >= MAX_METAMETHOD_DEPTH {
                    let error = self.error(ErrorKind::MetamethodDepthExceeded {
                        depth: self.metamethod_depth,
                        limit: MAX_METAMETHOD_DEPTH,
                    });
                    return Err(self.fail_call(error, idx, old_stack_bottom, charges));
                }
                self.metamethod_depth += 1;
                self.call_chain_depth += 1;
                *charges += 1;
            }
        }
    }

    /// Error exit of a call that started at `idx`: clear its slots, restore
    /// the caller's stack_bottom, release its `__call` chain charges, and
    /// report the error if this is the outermost host boundary.
    ///
    /// Errors from a non-function `__call` chain can pass through here
    /// without a Lua frame or RustFn reporting boundary. Releasing the
    /// charges first lets the outermost host call see the chain depth back at
    /// zero, so it reports the error once.
    #[cold]
    #[inline(never)]
    fn fail_call(
        &mut self,
        error: Error,
        idx: usize,
        old_stack_bottom: usize,
        charges: &mut u32,
    ) -> Error {
        self.truncate_call_frame(idx, old_stack_bottom);
        self.release_chain_charges(std::mem::take(charges));
        self.report_call_error(error)
    }

    fn release_chain_charges(&mut self, charges: u32) {
        self.metamethod_depth -= charges;
        self.call_chain_depth -= charges;
    }

    /// The one place a Rust callback is invoked. Every RustFn call - `call_op`
    /// and the generic-for fast path alike - goes through here, so the
    /// callback ABI (frame boundary, active function, result validation and
    /// error reporting) has a single implementation.
    ///
    /// Precondition: the callee's arguments occupy `stack[call_base..]` and
    /// the function value itself is no longer on the stack. On success the
    /// callback's `n` results occupy `stack[call_base..call_base + n]`, the
    /// caller's `stack_bottom` is restored, and `n` is returned. On error the
    /// stack is truncated to `call_base`, `stack_bottom` and the active Rust
    /// function are restored, and the error has passed through
    /// `report_call_error`.
    ///
    /// A callback that reports more results than its frame holds is rejected
    /// with `ResultCountExceedsStack` rather than padded with nil: the count
    /// is a host bug, and padding would hide it.
    ///
    /// Panics: a callback that panics poisons the State and the panic is
    /// resumed unchanged. A callback that returns (`Ok` or `Err`) on a State
    /// that became poisoned while it ran - it caught a panic from a nested
    /// call and swallowed it - does not get its result processed: execution
    /// is abandoned by unwinding with the `PoisonUnwind` marker, so no outer
    /// frame runs its ordinary cleanup against the abandoned bookkeeping.
    /// The public `State::guarded` boundary turns the marker into
    /// `StatePoisoned`.
    ///
    /// Kept out of line so the Rust-call machinery (its unwind landing pad
    /// included) does not enlarge `begin_call`'s frame, which stays live
    /// across every native re-entry.
    #[inline(never)]
    pub(super) fn invoke_rust_fn(
        &mut self,
        f: RustFunc,
        call_base: usize,
        method: bool,
    ) -> Result<u8> {
        let old_stack_bottom = self.stack_bottom;
        self.stack_bottom = call_base;

        // Invariant: the caller's active Rust function and stack_bottom are
        // both restored before a callback error propagates. Every stack index
        // and get_top() after this call is relative to stack_bottom, so the
        // callback result is handled explicitly rather than with `?`.
        let previous_rust_fn = self.active_rust_fn.replace(f);
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self))) {
            Ok(result) => result,
            Err(payload) => {
                self.poisoned = true;
                std::panic::resume_unwind(payload)
            }
        };
        if self.poisoned {
            Self::abandon_poisoned();
        }
        self.active_rust_fn = previous_rust_fn;
        let num_ret_reported = match result {
            Ok(n) => n,
            Err(e) => {
                self.truncate_call_frame(call_base, old_stack_bottom);
                // The argument numbering of an argument error is settled
                // here, by the call that directly invoked the callback.
                return Err(self.report_call_error(e.settle_method_call(method)));
            }
        };

        let available = self.get_top();
        let reported = usize::from(num_ret_reported);
        if reported > available {
            self.truncate_call_frame(call_base, old_stack_bottom);
            let error = self.error(ErrorKind::ResultCountExceedsStack {
                reported: num_ret_reported,
                available,
            });
            return Err(self.report_call_error(error));
        }
        if reported < available {
            // Keep the top `reported` values, in order, at call_base.
            self.stack[call_base..].rotate_right(reported);
            self.stack.truncate(call_base + reported);
        }
        self.stack_bottom = old_stack_bottom;
        Ok(num_ret_reported)
    }

    #[cold]
    #[inline(never)]
    fn abandon_poisoned() -> ! {
        std::panic::resume_unwind(Box::new(super::PoisonUnwind))
    }

    // Used only on error exits, never in the bytecode dispatch loop.
    fn report_call_error(&mut self, mut error: Error) -> Error {
        if self.is_outermost_host_boundary() {
            self.host_error(&mut error);
        }
        error
    }

    fn is_outermost_host_boundary(&self) -> bool {
        self.call_depth == 0 && self.call_chain_depth == 0 && self.active_rust_fn.is_none()
    }

    fn truncate_call_frame(&mut self, call_base: usize, old_stack_bottom: usize) {
        self.stack.truncate(call_base);
        self.stack_bottom = old_stack_bottom;
    }

    /// Pops `n` values from the stack, concatenates them, and pushes the
    /// result. `n` must be at least 2; a smaller count, or one larger than the
    /// current frame's values, is rejected with `InvalidStackIndex` (the same
    /// kind `pop` uses for a bad count). The error carries `-n`, saturated
    /// at `isize::MIN` when the count is too large.
    pub fn concat(&mut self, n: usize) -> Result<()> {
        // The count is the problem, not a Lua argument, so this is not an
        // ArgError: `InvalidStackIndex` is what the other count-taking host
        // APIs return.
        self.check_poisoned()?;
        if n < 2 || n > self.get_top() {
            return Err(self.error(ErrorKind::InvalidStackIndex {
                index: isize::try_from(n).map_or(isize::MIN, |n| -n),
            }));
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
                total_len = crate::limits::checked_string_growth(total_len, s.len())?;
            } else if let Some(num) = val.as_num() {
                // Use the same spelling as tostring and default coercion.
                let rendered = super::lua_val::number_to_string(num);
                total_len = crate::limits::checked_string_growth(total_len, rendered.len())?;
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
        self.push_unchecked(val); // Replaces the concatenation operands just removed.
        Ok(())
    }

    /// Admit a Lua call: check the call depth, push the activation record,
    /// and lay out the callee's frame above the arguments, whose first slot
    /// becomes its stack_bottom. The slot just below holds the callee value,
    /// which keeps it rooted while it runs; the results are returned into
    /// that slot upwards.
    ///
    /// On error nothing stays admitted: the call stack, call depth and
    /// stack_bottom are restored and the frame's slots cleared.
    pub(super) fn admit_lua_call(
        &mut self,
        bytecode: Arc<Bytecode>,
        num_args: u8,
        old_stack_bottom: usize,
    ) -> Result<()> {
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(self.error(ErrorKind::CallDepthExceeded {
                depth: self.call_depth,
                limit: MAX_CALL_DEPTH,
            }));
        }
        self.call_depth += 1;
        let frame_bottom = self.stack.len() - num_args as usize;
        self.stack_bottom = frame_bottom;

        let num_params = bytecode.num_params;
        let num_locals = bytecode.num_locals;
        let is_vararg = bytecode.is_vararg;

        // Extra arguments of a vararg function leave the value stack for the
        // activation record, which roots them.
        let has_varargs = is_vararg && num_args > num_params;
        if has_varargs {
            let num_varargs = (num_args - num_params) as usize;
            let varargs = self.collect_varargs(self.stack.len() - num_varargs);
            self.vararg_stack.push(varargs);
        }
        // Extended from `once_with` so the record is written straight into
        // its slot; see the frame push in `drive_invocation` for why a plain
        // `push` does not.
        self.call_stack.extend(std::iter::once_with(|| CallInfo {
            bytecode,
            ip: 0,
            chain_charges: 0,
            has_varargs,
        }));

        // Check stack space for parameters and locals
        let extra_params = if num_args < num_params {
            (num_params - num_args) as usize
        } else {
            0
        };
        if let Err(e) = self.check_stack_space(extra_params + num_locals as usize) {
            return Err(self.reject_admission(e, frame_bottom, old_stack_bottom));
        }

        match num_args.cmp(&num_params) {
            Ordering::Less => {
                for _ in num_args..num_params {
                    self.push_unchecked(Val::Nil);
                }
            }
            Ordering::Greater => {
                // A public fixed call prevalidates these argument slots, so
                // this pop cannot fail through the host API. Keep reporting
                // here for a future internal caller that violates that invariant.
                if !is_vararg && let Err(e) = self.pop_n((num_args - num_params) as isize) {
                    return Err(self.reject_admission(e, frame_bottom, old_stack_bottom));
                }
                // If is_vararg, we already collected the extra args above
            }
            Ordering::Equal => (),
        }

        for _ in 0..num_locals {
            self.push_unchecked(Val::Nil);
        }

        Ok(())
    }

    /// The bytecode of `func` if it is a Lua function, without cloning the
    /// rest of its closure.
    fn lua_function_bytecode(&self, func: Val) -> Option<Arc<Bytecode>> {
        match func {
            Val::Obj(ptr) => self
                .heap
                .as_lua_function_ref(ptr)
                .map(|closure| Arc::clone(&closure.bytecode)),
            _ => None,
        }
    }

    /// The frame of the activation just admitted, built from the closure in
    /// its function slot.
    pub(super) fn admitted_frame(&self) -> Frame {
        let closure = self.stack[self.stack_bottom - 1]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_lua_function_ref(ptr))
            .expect("an admitted Lua activation's function slot holds its closure");
        Frame::new(
            Arc::clone(&closure.bytecode),
            Arc::clone(&closure.runtime),
            Arc::clone(&closure.upvalues),
            self.stack_bottom,
            self.pending_bases.floor(),
        )
    }

    /// Drain the frame's extra arguments into the per-activation vararg Vec.
    ///
    /// A one-line helper so the allocation has its own hotpath measurement
    /// point. `#[inline]` keeps plain builds at the original codegen.
    #[hotpath::measure]
    #[inline]
    fn collect_varargs(&mut self, vararg_start: usize) -> Vec<Val> {
        self.stack.drain(vararg_start..).collect()
    }

    /// Report an error raised after admission but before the frame starts
    /// executing, then undo the admission.
    ///
    /// Out of line and cold so the temporary `Frame` built for the traceback
    /// stays off the call path.
    #[cold]
    #[inline(never)]
    fn reject_admission(
        &mut self,
        error: Error,
        frame_bottom: usize,
        old_stack_bottom: usize,
    ) -> Error {
        let frame = self.admitted_frame();
        let error = self.report_frame_error(error, &frame);
        self.stack.truncate(frame_bottom);
        self.stack_bottom = old_stack_bottom;
        let _charges = self.pop_activation();
        error
    }

    /// Run the Lua activation just admitted to completion in a fresh
    /// dispatch-loop invocation. Lua calls it makes run in the same
    /// invocation; only calls made from Rust start another (see
    /// `call_unguarded`). On success the activation's results sit from its
    /// function slot upwards and the caller's stack_bottom is restored.
    ///
    /// The invocation keeps its frames in a stack of its own, borrowed from
    /// `State::frame_stacks` so a re-entry allocates nothing in steady
    /// state. A frame stays where it was pushed until it returns: moving a
    /// frame the loop has been writing field by field made the optimizer
    /// read it back with wide loads that cannot be store-forwarded, a stall
    /// on every call and return. The invocation owns the activation records
    /// from its entry upwards: on error it unwinds exactly those, never an
    /// outer invocation's.
    pub(super) fn run_invocation(&mut self, old_stack_bottom: usize) -> Result<u8> {
        let mut frames = self.frame_stacks.pop().unwrap_or_default();
        frames.push(self.admitted_frame());
        let result = match Frame::run(&mut frames, self) {
            Ok(ret_count) => {
                let frame = frames.last().expect("the entry frame returned");
                match self.collect_returns(frame, ret_count) {
                    Ok(count) => {
                        let charges = self.pop_activation();
                        // An entry activation's charges stay with its Rust
                        // caller, which also adjusts the results.
                        debug_assert_eq!(charges, 0);
                        frames.clear();
                        self.stack_bottom = old_stack_bottom;
                        Ok(count)
                    }
                    Err(error) => Err(self.unwind_invocation(&mut frames, error, old_stack_bottom)),
                }
            }
            Err(error) => Err(self.unwind_invocation(&mut frames, error, old_stack_bottom)),
        };
        debug_assert!(frames.is_empty());
        self.frame_stacks.push(frames);
        result
    }

    /// `OP_RETURN` from a frame whose caller is a Lua frame of the same
    /// invocation: finish the results, pop the activation and its frame, and
    /// adjust the results to the count the caller's `OP_CALL` expects,
    /// releasing the charges the callee's `__call` resolution held. A failure
    /// to finish the results belongs to the returning frame (still on top);
    /// a failure to pad them belongs to the caller's call instruction, as it
    /// does for a call made from Rust.
    ///
    /// Records and frames are dropped in place, never moved out: the
    /// dispatch loop wrote their fields one at a time just before, and a
    /// whole-struct move reads them back with wide loads that cannot be
    /// store-forwarded from those writes. Out of line so the return
    /// machinery stays out of the dispatch loop's code.
    #[inline(never)]
    pub(super) fn return_to_caller(
        &mut self,
        frames: &mut Vec<Frame>,
        ret_count: RetCount,
    ) -> Result<()> {
        let frame = frames.last().expect("a returning frame is on its stack");
        let count = self.collect_returns(frame, ret_count)?;
        // The returning frame's function slot, which now holds its first
        // result.
        let call_base = frame.stack_bottom - 1;
        let charges = self.pop_activation();
        frames.truncate(frames.len() - 1);
        let caller = frames.last().expect("a nested return has a Lua caller");
        self.stack_bottom = caller.stack_bottom;
        let expected = caller.pending_call_results();
        self.finish_call(call_base, caller.stack_bottom, expected, count, charges)
    }

    /// Unwind every frame of one dispatch-loop invocation after an error,
    /// innermost first, giving each the cleanup a returning frame gets: its
    /// pending bases dropped, the error located and reported
    /// (`report_frame_error` keeps whatever location, fault and traceback the
    /// error already carries, and reports it to the host once), its upvalues
    /// closed before its slots go, and its `__call` chain charges released
    /// before the caller sees the error.
    #[cold]
    #[inline(never)]
    fn unwind_invocation(
        &mut self,
        frames: &mut Vec<Frame>,
        mut error: Error,
        old_stack_bottom: usize,
    ) -> Error {
        while let Some(frame) = frames.last() {
            self.pending_bases.truncate(frame.pending_floor);
            error = self.report_frame_error(error, frame);
            let frame_bottom = frame.stack_bottom;
            self.close_upvalues(frame_bottom);
            self.stack.truncate(frame_bottom);
            let charges = self.pop_activation();
            self.release_chain_charges(charges);
            frames.truncate(frames.len() - 1);
            if let Some(caller) = frames.last() {
                self.stack_bottom = caller.stack_bottom;
            }
        }
        self.stack_bottom = old_stack_bottom;
        error
    }

    /// Pop the innermost activation record and its varargs, and return the
    /// `__call` chain charges it held for the caller to release.
    fn pop_activation(&mut self) -> u32 {
        let len = self.call_stack.len();
        let info = &self.call_stack[len - 1];
        let charges = info.chain_charges;
        if info.has_varargs {
            self.vararg_stack.pop();
        }
        self.call_stack.truncate(len - 1);
        self.call_depth -= 1;
        charges
    }

    /// Finish a returning frame's results: count them, close the frame's
    /// upvalues, and slide the results down into its function slot, just
    /// below its stack_bottom, and upwards. Fails, before changing anything,
    /// only when a dynamic count exceeds `MAX_RESULTS`; that error belongs to
    /// the returning frame.
    fn collect_returns(&mut self, frame: &Frame, ret_count: RetCount) -> Result<u8> {
        // A balanced frame consumed every base it pushed, so this is a no-op.
        self.pending_bases.truncate(frame.pending_floor);
        // Handle RetCount::All which means "return all values on stack"
        let actual_num_returned = match ret_count {
            RetCount::All => {
                // Calculate how many values are above the frame's locals
                let frame_base = frame.stack_bottom
                    + frame.bytecode().num_params as usize
                    + frame.bytecode().num_locals as usize;
                let count = self.stack.len() - frame_base;
                crate::error::results_count(count).map_err(|kind| self.error(kind))?
            }
            RetCount::Fixed(n) => n,
        };

        // Close open upvalues BEFORE moving anything: they point at the
        // frame's local slots, which sit below `ret_start` and must still
        // hold their values when the close reads them. The returns are
        // transient operands above the locals, so no upvalue can point at
        // them.
        self.close_upvalues(frame.stack_bottom);

        // Slide the return values down over the frame in one memmove: drain
        // the function slot, params, locals and operands beneath them and let
        // the tail shift. The stack only ever shrinks here, so the cap needs
        // no preflight.
        let ret_start = self.stack.len() - actual_num_returned as usize;
        self.stack.drain(frame.stack_bottom - 1..ret_start);
        Ok(actual_num_returned)
    }

    /// Fill in a runtime error's source line from the frame it surfaced in.
    ///
    /// Every error-return path out of a Lua frame must go through this, so that
    /// the line rendered alongside the traceback's source always comes from the
    /// same frame. An error that already carries a location keeps it - that is
    /// how parser errors survive with their own line and column.
    fn locate_in_frame(error: Error, frame: &Frame) -> Error {
        if error.line_num() == 0 {
            let column = error.column();
            error.with_location(frame.current_line(), column)
        } else {
            error
        }
    }

    #[cold]
    #[inline(never)]
    fn report_frame_error(&mut self, mut error: Error, frame: &Frame) -> Error {
        // Preserve the innermost fault even when a callback already attached a
        // traceback. Allocate metadata only here, outside the dispatch loop.
        error = frame.locate_fault(error);
        if error.stack_trace().is_empty() {
            error = Self::locate_in_frame(error, frame);
            error = error.with_stack_trace(self.build_stack_trace(frame));
        }
        self.host_error(&mut error);
        error
    }

    pub(crate) fn push_chunk(&mut self, bytecode: Arc<Bytecode>) -> Result<()> {
        self.push_closure(bytecode, Vec::new())
    }

    #[hotpath::measure]
    pub(super) fn push_closure(
        &mut self,
        bytecode: Arc<Bytecode>,
        upvalues: Vec<UpvalueRef>,
    ) -> Result<()> {
        self.check_stack_space(1)?;
        let runtime = self.resolve_bytecode_runtime(&bytecode)?;
        let identity = Arc::as_ptr(&bytecode) as usize;
        // No host callback or fallible operation occurs from this push through
        // its matching pop. The allocator's collection check must see the
        // pending identity so its cache sweep retains the runtime before the
        // closure becomes a root; the allocation itself never collects.
        // Nested captures finish beforehand.
        self.pending_bytecode_caches.push(identity);
        let obj = self.allocator().alloc_lua_fn(bytecode, runtime, upvalues);
        let pending = self
            .pending_bytecode_caches
            .pop()
            .expect("pending bytecode cache missing after closure allocation");
        debug_assert_eq!(pending, identity);
        self.push_unchecked(Val::Obj(obj));
        Ok(())
    }

    /// Find an existing open upvalue for the given stack index, or create a new one.
    #[hotpath::measure]
    pub(super) fn find_or_create_upvalue(&mut self, stack_idx: usize) -> UpvalueRef {
        // Binary search preserves identity on hits and the exact ascending
        // insertion/descending close order on misses. Vec insertion still
        // shifts entries, but changing the close order would change pool reuse.
        // The shift is bounded by the frame width, not by call depth: only the
        // executing frame captures new locals, every frame exit closes what it
        // opened, so entries above the insertion point are this frame's own
        // (at most MAX_FRAME_SLOTS - 1 of them).
        let pos = match self
            .open_upvalues
            .binary_search_by_key(&stack_idx, |(idx, _)| *idx)
        {
            Ok(pos) => return self.open_upvalues[pos].1,
            Err(pos) => pos,
        };
        let uv_ref = self.upvalue_pool.alloc(Upvalue::Open(stack_idx));
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
        // Place a real closure in the function slot below the arguments, as
        // a call would have it.
        let bytecode = Arc::new(bytecode);
        let func_slot = self.stack.len() - num_args as usize;
        self.push_chunk(Arc::clone(&bytecode))?;
        let func = self.pop_val();
        self.stack.insert(func_slot, func);
        let old_stack_bottom = self.stack_bottom;
        self.admit_lua_call(bytecode, num_args, old_stack_bottom)?;
        self.run_invocation(old_stack_bottom)
    }
}
