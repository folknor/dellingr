//! Evaluation and function call logic for the Lua VM.
//!
//! This module contains methods for calling functions, evaluating closures,
//! and managing the call stack.

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
    /// Once the call operands pass validation, an error removes the callee,
    /// its arguments and any partial results, preserving earlier stack slots.
    /// Host-misuse preflight errors leave the stack unchanged. Writes made by
    /// the callee survive an error.
    ///
    /// A poisoned State (see [`State::is_poisoned`]) is refused with
    /// `StatePoisoned`. A panic out of the callee poisons the State and
    /// propagates; a callback that catches a panic and returns normally gets
    /// its execution abandoned, and this call returns `StatePoisoned`.
    pub fn call(&mut self, num_args: ArgCount, num_ret_expected: RetCount) -> Result<()> {
        // native-call: call->call_unguarded
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
    ///
    /// Callees own only slots at or above their frame bottom. Public stack
    /// operations enforce that isolation, so a return or failure preserves
    /// the caller's slot positions even if pushes reallocate the buffer.
    /// Explicit upvalue writes can still change captured caller values.
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
        #[cfg(not(feature = "fault-injection"))]
        let injected = false;
        #[cfg(feature = "fault-injection")]
        let injected = self.inject_fault(
            "native_depth",
            self.native_call_depth as usize,
            self.native_call_depth > 0,
        );
        let result = if injected || self.native_call_depth >= MAX_NATIVE_CALL_DEPTH {
            // Only a nested call can exhaust the native bracket. Its enclosing
            // activation or callback owns reporting, through the common arm.
            Err(self.error(ErrorKind::NativeCallDepthExceeded {
                depth: self.native_call_depth,
                limit: MAX_NATIVE_CALL_DEPTH,
            }))
        } else {
            self.native_call_depth += 1;
            // Each `Result` here dies in its own `let` before the next step
            // runs: a `match` scrutinee would stay live across
            // `run_invocation`, and its slot then cannot be shared with
            // `Frame::run`'s result, which grows this frame, live across
            // every native re-entry, in release.
            let result = 'call: {
                // native-call: call_unguarded->begin_call
                let admitted = match self.begin_call(idx, num_args, old_stack_bottom, false) {
                    Ok(admitted) => admitted,
                    Err(error) => break 'call Err(error),
                };
                let num_ret_actual = match admitted {
                    Some(n) => n,
                    // native-call: call_unguarded->run_invocation
                    None => match self.run_invocation(old_stack_bottom) {
                        Ok(n) => n,
                        Err(error) => break 'call Err(error),
                    },
                };
                self.finish_call(num_ret_expected, num_ret_actual)
            };
            self.native_call_depth -= 1;
            result
        };
        match result {
            Ok(()) => Ok(()),
            Err(error) => Err(self.fail_call(error, idx, old_stack_bottom)),
        }
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

    /// A call made by the dispatch loop's `OP_CALL`. A Rust callee runs to
    /// completion here and `false` is returned. A Lua callee is admitted
    /// (activation record pushed, frame slots laid out, with chain charges
    /// handed to the record by begin_callable_call) and `true` is returned.
    /// The loop exits to its driver, which pushes the callee's frame and runs
    /// it without native recursion.
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
        // native-call: call_from_lua->begin_call
        match self.begin_call(idx, num_args, old_stack_bottom, method)? {
            Some(n) => {
                self.finish_call(num_ret_expected, n)?;
                Ok(false)
            }
            None => Ok(true),
        }
    }

    /// Adjust a completed call's results. Chain owners have already released
    /// their units; any padding error propagates to the boundary or unwind.
    fn finish_call(&mut self, expected: RetCount, actual: u8) -> Result<()> {
        if let RetCount::Fixed(expected) = expected {
            if expected < actual {
                self.stack
                    .truncate(self.stack.len() - usize::from(actual - expected));
            } else if expected > actual {
                self.balance_stack(usize::from(expected), usize::from(actual))?;
            }
        }
        Ok(())
    }

    /// Resolve the callable at `idx` (with `num_args` arguments above it)
    /// and start the call. A Rust callee runs to completion and its result
    /// count is returned; a Lua callee is admitted (activation record pushed,
    /// frame slots laid out) but not run, and `None` is returned.
    ///
    /// Non-function resolution owns its chain units locally, releasing on
    /// a Rust return or transferring them to an admitted Lua activation.
    /// Errors propagate to call_unguarded or the dispatch-loop unwind;
    /// stack_bottom is restored by the function that moved it.
    #[inline(always)]
    fn begin_call(
        &mut self,
        idx: usize,
        num_args: u8,
        old_stack_bottom: usize,
        method: bool,
    ) -> Result<Option<u8>> {
        // The two function cases inline into the call sites; only a
        // callable object pays for the resolution loop's setup.
        let func_val = self.stack[idx];
        if let Val::RustFn(f) = func_val {
            self.stack.remove(idx);
            // native-call: begin_call->invoke_rust_fn
            return self.invoke_rust_fn(f, idx, method).map(Some);
        }
        if let Some(bytecode) = self.lua_function_bytecode(func_val) {
            self.admit_lua_call(bytecode, num_args, old_stack_bottom)?;
            return Ok(None);
        }
        // native-call: begin_call->begin_callable_call
        self.begin_callable_call(idx, num_args, old_stack_bottom, method)
    }

    /// `begin_call` for a callee that is not a function: the general
    /// resolution loop, which also handles the function cases it resolves to.
    #[inline(never)]
    fn begin_callable_call(
        &mut self,
        idx: usize,
        num_args: u8,
        old_stack_bottom: usize,
        method: bool,
    ) -> Result<Option<u8>> {
        let mut num_args = num_args;
        let mut charges = 0;
        let result = 'resolve: loop {
            let func_val = self.stack[idx];
            if let Val::RustFn(f) = func_val {
                // The Rust callback ABI: the callee's frame starts at the
                // first argument, with the function value gone.
                self.stack.remove(idx);
                // native-call: begin_callable_call->invoke_rust_fn
                break self.invoke_rust_fn(f, idx, method).map(Some);
            }
            if let Some(bytecode) = self.lua_function_bytecode(func_val) {
                // A Lua callee keeps its function slot: the value stays
                // rooted there below the callee's frame, its frame is built
                // from it (`State::admitted_frame`), and the results land in
                // it on return.
                match self.admit_lua_call(bytecode, num_args, old_stack_bottom) {
                    Ok(()) => {
                        self.call_stack
                            .last_mut()
                            .expect("admission created an activation")
                            .chain_charges = charges;
                        charges = 0;
                        break Ok(None);
                    }
                    Err(error) => break Err(error),
                }
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
                break 'resolve Err(error);
            }
            // Insert the receiver as first argument and call the handler.
            // Stack is currently: [callable, arg1, arg2, ..., argN]
            // We need: [handler, receiver, arg1, arg2, ..., argN]
            let Some(combined) = num_args.checked_add(1) else {
                let error = self.error(ErrorKind::TooManyArguments {
                    limit: crate::limits::MAX_ARGUMENTS,
                });
                break 'resolve Err(error);
            };
            // The handler takes the callable's slot and the receiver is
            // inserted after it: exactly one slot more than the call began
            // with.
            if let Err(e) = self.check_stack_space(1) {
                break 'resolve Err(e);
            }
            self.stack[idx] = call_handler;
            self.stack.insert(idx + 1, func_val);
            num_args = combined;
            let handler_is_function = matches!(call_handler, Val::RustFn(_))
                || call_handler.as_lua_function(&self.heap).is_some();
            if !handler_is_function {
                #[cfg(not(feature = "fault-injection"))]
                let injected = false;
                #[cfg(feature = "fault-injection")]
                let injected =
                    self.inject_fault("call_chain", self.metamethod_depth as usize, true);
                if injected || self.metamethod_depth >= MAX_METAMETHOD_DEPTH {
                    let error = self.error(ErrorKind::MetamethodDepthExceeded {
                        depth: self.metamethod_depth,
                        limit: MAX_METAMETHOD_DEPTH,
                    });
                    break 'resolve Err(error);
                }
                self.metamethod_depth += 1;
                charges += 1;
            }
        };
        self.release_chain_charges(charges);
        result
    }

    /// The single error arm for a call made from Rust. Inner owners have
    /// restored their bookkeeping; this boundary clears the operand suffix
    /// and reports errors that have no enclosing activation or Rust callback.
    #[cold]
    #[inline(never)]
    fn fail_call(&mut self, error: Error, idx: usize, old_stack_bottom: usize) -> Error {
        debug_assert!(self.stack.len() >= idx);
        debug_assert_eq!(self.stack_bottom, old_stack_bottom);
        debug_assert!(self.open_upvalues.iter().all(|(slot, _)| *slot < idx));
        self.stack.truncate(idx);
        self.stack_bottom = old_stack_bottom;
        self.report_call_error(error)
    }

    fn release_chain_charges(&mut self, charges: u32) {
        self.metamethod_depth -= charges;
    }

    /// The one place a Rust callback is invoked. Every RustFn call - `call_op`
    /// and the generic-for fast path alike - goes through here, so the
    /// callback ABI (frame boundary, active function and result validation)
    /// has a single implementation. The enclosing call boundary or invocation
    /// unwind owns error reporting.
    ///
    /// Precondition: the callee's arguments occupy `stack[call_base..]` and
    /// the function value itself is no longer on the stack. On success the
    /// callback's `n` results occupy `stack[call_base..call_base + n]`, the
    /// caller's `stack_bottom` is restored, and `n` is returned. On error the
    /// stack_bottom and the active Rust function are restored; callee slots
    /// stay for the enclosing boundary or unwind to clear and report there.
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
                self.stack_bottom = old_stack_bottom;
                // The argument numbering of an argument error is settled
                // here, by the call that directly invoked the callback.
                return Err(e.settle_method_call(method));
            }
        };

        let available = self.get_top();
        let reported = usize::from(num_ret_reported);
        if reported > available {
            self.stack_bottom = old_stack_bottom;
            let error = self.error(ErrorKind::ResultCountExceedsStack {
                reported: num_ret_reported,
                available,
            });
            return Err(error);
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
        // Any still-held chain units belong to an enclosing activation or
        // Rust callback, already excluded by these two checks.
        self.call_depth == 0 && self.active_rust_fn.is_none()
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
        #[cfg(not(feature = "fault-injection"))]
        let injected = false;
        #[cfg(feature = "fault-injection")]
        let injected = self.inject_fault("call_depth", self.call_depth as usize, true);
        if injected || self.call_depth >= MAX_CALL_DEPTH {
            return Err(self.error(ErrorKind::CallDepthExceeded {
                depth: self.call_depth,
                limit: MAX_CALL_DEPTH,
            }));
        }
        self.call_depth += 1;
        let frame_bottom = self.stack.len() - num_args as usize;
        self.stack_bottom = frame_bottom;

        let num_params = bytecode.num_params;
        let max_stack = usize::from(bytecode.max_stack);
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

        // A public fixed call prevalidates these argument slots, so this pop
        // cannot fail through the host API. Keep reporting here for a future
        // internal caller that violates that invariant. A vararg function's
        // extra arguments were already collected above.
        if num_args > num_params
            && !is_vararg
            && let Err(e) = self.pop_n((num_args - num_params) as isize)
        {
            return Err(self.reject_admission(e, frame_bottom, old_stack_bottom));
        }

        // The frame reserves every register it can use, parameters, locals
        // and temporaries, against the stack cap now, so its bytecode never
        // checks it again. Missing arguments and every other register start
        // nil.
        let frame_top = frame_bottom + max_stack;
        let grow = frame_top.saturating_sub(self.stack.len());
        if let Err(e) = self.check_stack_space(grow) {
            return Err(self.reject_admission(e, frame_bottom, old_stack_bottom));
        }
        self.stack.resize(frame_top, Val::Nil);

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
        // The chain transfer happens after admission, so a rejected record
        // never holds units.
        debug_assert_eq!(
            self.call_stack
                .last()
                .expect("a rejected admission pushed its record")
                .chain_charges,
            0
        );
        self.pop_activation();
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
        // native-call: run_invocation->run
        let result = match Frame::run(&mut frames, self) {
            Ok((first, count)) => {
                let frame = frames.last().expect("the entry frame returned");
                match self.collect_returns(frame, first, count) {
                    Ok(count) => {
                        self.pop_activation();
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
    /// release its chain charges, then adjust results to the caller's
    /// `OP_CALL` count. A failure
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
        first: usize,
        count: usize,
    ) -> Result<()> {
        let frame = frames.last().expect("a returning frame is on its stack");
        let count = self.collect_returns(frame, first, count)?;
        self.pop_activation();
        frames.truncate(frames.len() - 1);
        let caller = frames.last().expect("a nested return has a Lua caller");
        self.stack_bottom = caller.stack_bottom;
        let expected = caller.pending_call_results();
        self.finish_call(expected, count)?;
        // A fixed result count resumes ordinary execution at the caller's
        // frame top; all results stay open for the consuming instruction.
        if let RetCount::Fixed(_) = expected {
            let top = caller.top;
            self.stack.resize(top, Val::Nil);
        }
        Ok(())
    }

    /// Unwind every frame of one dispatch-loop invocation after an error,
    /// innermost first, giving each the cleanup a returning frame gets: the
    /// error located and reported
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
            error = self.report_frame_error(error, frame);
            let frame_bottom = frame.stack_bottom;
            self.close_upvalues(frame_bottom);
            self.stack.truncate(frame_bottom);
            self.pop_activation();
            frames.truncate(frames.len() - 1);
            if let Some(caller) = frames.last() {
                self.stack_bottom = caller.stack_bottom;
            }
        }
        self.stack_bottom = old_stack_bottom;
        error
    }

    /// Pop the innermost activation record and its varargs, releasing the
    /// `__call` chain charges the record held.
    ///
    /// The release comes first, and stays inside: the record owns the
    /// units, so popping it is what releases them, and a count returned for
    /// the caller to release would be live across the record's drop (an
    /// `Arc` decrement and the vararg Vec's free) in a callee-saved register.
    /// In `run_invocation`, inlined into `call_unguarded`, that one extra
    /// live value spilled `old_stack_bottom` and split the result slot
    /// `Frame::run` and `begin_callable_call` had shared, growing a frame
    /// that is live across every native re-entry by 32 bytes in release.
    fn pop_activation(&mut self) {
        let len = self.call_stack.len();
        let info = &self.call_stack[len - 1];
        self.metamethod_depth -= info.chain_charges;
        if info.has_varargs {
            self.vararg_stack.pop();
        }
        self.call_stack.truncate(len - 1);
        self.call_depth -= 1;
    }

    /// Finish a returning frame's results, the `count` values from register
    /// index `first`: close the frame's upvalues, and slide the results down
    /// into its function slot, just below its stack_bottom, and upwards.
    /// Fails, before changing anything, only when the count exceeds
    /// `MAX_RESULTS`; that error belongs to the returning frame.
    fn collect_returns(&mut self, frame: &Frame, first: usize, count: usize) -> Result<u8> {
        let returned = crate::error::results_count(count).map_err(|kind| self.error(kind))?;

        // Close open upvalues BEFORE moving anything: results can come
        // straight from locals, and every captured local must still hold its
        // value when the close reads it.
        self.close_upvalues(frame.stack_bottom);

        // Drop the registers above the results, then slide the results down
        // over the frame in one memmove: drain the function slot and every
        // register beneath them and let the tail shift. The stack only ever
        // shrinks here, so the cap needs no preflight.
        self.stack.truncate(first + count);
        self.stack.drain(frame.stack_bottom - 1..first);
        Ok(returned)
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
        let runtime_ptr = Arc::as_ptr(&runtime);
        let obj = self.allocator().alloc_lua_fn(bytecode, runtime, upvalues);
        // A collection inside `allocator` must have kept this runtime as the
        // cached one, or the weak cache processing would no longer reach it.
        debug_assert!(
            self.bytecode_caches
                .get(&identity)
                .is_some_and(|entry| std::ptr::eq(Arc::as_ptr(&entry.runtime), runtime_ptr))
        );
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
