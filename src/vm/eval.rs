//! Evaluation and function call logic for the Lua VM.
//!
//! This module contains methods for calling functions, evaluating closures,
//! and managing the call stack.

use std::sync::Arc;

use super::RustFnId;
use super::frame::Frame;
use super::lua_val::Val;
use super::object::{Upvalue, UpvalueRef};
use super::{
    Bytecode, CallInfo, ChainUnits, Error, ErrorKind, MAX_CALL_DEPTH, Result, State, TypeError,
};
use crate::instr::{ArgCount, RetCount};
use crate::limits::{MAX_ARGUMENTS, MAX_METAMETHOD_DEPTH, MAX_NATIVE_CALL_DEPTH};

/// Stack slots a Lua frame's admission reserves, unfilled, above its
/// registers: the largest call window an instruction pushes there, the
/// `__newindex` function handler's `[handler, table, key, value]`. The
/// `__index`, `__len` and generic-for iterator windows take three. Reference
/// Lua reserves `EXTRA_STACK` for the same purpose.
pub(super) const FRAME_HEADROOM: usize = 4;

/// Entry observation is zero-sized for ordinary calls, including debug builds.
/// Only direct function-valued `__index` and `__newindex` handlers carry a
/// mutable commit flag.
pub(crate) trait CallEntry {
    fn mark_entered(self);
    fn tracks_entry(&self) -> bool;
}

impl CallEntry for () {
    fn mark_entered(self) {}
    fn tracks_entry(&self) -> bool {
        false
    }
}

impl CallEntry for &mut bool {
    fn mark_entered(self) {
        *self = true;
    }
    fn tracks_entry(&self) -> bool {
        true
    }
}

/// Call operands with an optional, statically selected entry observer.
pub(crate) trait CallArguments {
    type Entry: CallEntry;
    fn into_parts(self) -> (ArgCount, Self::Entry);
}

impl CallArguments for ArgCount {
    type Entry = ();
    fn into_parts(self) -> (ArgCount, ()) {
        (self, ())
    }
}

impl<'a> CallArguments for (ArgCount, &'a mut bool) {
    type Entry = &'a mut bool;
    fn into_parts(self) -> (ArgCount, Self::Entry) {
        self
    }
}

/// The `__newindex` handler's entry observer: the same commit flag as the
/// `&mut bool` one `get_table_unguarded` uses, as its own type, so the host
/// read and the host store each get a `call_unguarded` instance with one
/// caller that stays inlined. Sharing one instance outlined it and added its
/// whole frame to every native level of every read path (128 bytes in
/// release; reference/performance.md).
pub(crate) struct StoreEntry<'a>(pub(crate) &'a mut bool);

impl CallEntry for StoreEntry<'_> {
    fn mark_entered(self) {
        *self.0 = true;
    }
    fn tracks_entry(&self) -> bool {
        true
    }
}

impl<'a> CallArguments for (ArgCount, StoreEntry<'a>) {
    type Entry = StoreEntry<'a>;
    fn into_parts(self) -> (ArgCount, Self::Entry) {
        self
    }
}

/// What kind of call a callee is being resolved for: a method call
/// (`obj:m()`, which numbers a library function's argument errors without
/// the receiver) and a tail call (whose admission replaces the calling
/// activation). Two flag bits in one byte, so the tail flag adds no argument
/// to the resolution path: a struct of two `bool`s is passed as two
/// arguments, and as the seventh it went on the stack in `call_unguarded`'s
/// frame, which is live across every native re-entry.
#[derive(Clone, Copy)]
pub(super) struct CallShape(u8);

impl CallShape {
    const METHOD: u8 = 1;
    const TAIL: u8 = 2;

    /// A call made from Rust: neither a method call nor a tail call.
    const HOST: Self = Self(0);

    /// A call made by a Lua call instruction. Arithmetic on the flags, not
    /// a branch on them: branching made `call_from_lua` duplicate its whole
    /// body per method flag.
    fn from_lua(method: bool, tail: bool) -> Self {
        Self((u8::from(method) * Self::METHOD) | (u8::from(tail) * Self::TAIL))
    }

    const fn method(self) -> bool {
        self.0 & Self::METHOD != 0
    }

    const fn tail(self) -> bool {
        self.0 & Self::TAIL != 0
    }
}

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
        self.guarded(|state| {
            crate::native_call!(
                "call->call_unguarded",
                state.call_unguarded(num_args, num_ret_expected)
            )
        })
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
    /// A direct index handler may supply an entry observer with its count;
    /// admission refusals leave that observer untouched.
    pub(crate) fn call_unguarded(
        &mut self,
        num_args: impl CallArguments,
        num_ret_expected: RetCount,
    ) -> Result<()> {
        // ArgCount::Dynamic is resolved by the executing Lua Frame from the
        // number of stack values above the callee. A host call has no bytecode
        // open tail to resolve, so the public entry point rejects Dynamic
        // before touching any state.
        //
        // This and the InvalidStackIndex preflight below are returned
        // without notifying on_error: they are host misuse detected before
        // anything executes, which the on_error doc in host.rs excludes by
        // contract. Every failure after dispatch reaches an error-reporting
        // boundary.
        let (count, entered) = num_args.into_parts();
        let num_args = match count {
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
        let old_stack_bottom = self.in_flight.stack_bottom;
        #[cfg(not(feature = "fault-injection"))]
        let injected = false;
        #[cfg(feature = "fault-injection")]
        let injected = self.inject_fault(
            crate::FaultSite::NativeDepth,
            self.in_flight.native_call_depth as usize,
            self.in_flight.native_call_depth > 0,
        );
        let result = if injected || self.in_flight.native_call_depth >= MAX_NATIVE_CALL_DEPTH {
            // Only a nested call can exhaust the native bracket. The common
            // arm reports it like any other failure after dispatch.
            Err(self.error(ErrorKind::NativeCallDepthExceeded {
                depth: self.in_flight.native_call_depth,
                limit: MAX_NATIVE_CALL_DEPTH,
            }))
        } else {
            self.in_flight.native_call_depth += 1;
            // Each `Result` here dies in its own `let` before the next step
            // runs: a `match` scrutinee would stay live across
            // `run_invocation`, and its slot then cannot be shared with
            // `Frame::run`'s result, which grows this frame, live across
            // every native re-entry, in release.
            let result = 'call: {
                let admitted = match crate::native_call!(
                    "call_unguarded->begin_call",
                    self.begin_call(idx, num_args, old_stack_bottom, CallShape::HOST, entered)
                ) {
                    Ok(admitted) => admitted,
                    Err(error) => break 'call Err(error),
                };
                let num_ret_actual = match admitted {
                    Some(n) => n,
                    None => match crate::native_call!(
                        "call_unguarded->run_invocation",
                        self.run_invocation(old_stack_bottom)
                    ) {
                        Ok(n) => n,
                        Err(error) => break 'call Err(error),
                    },
                };
                self.finish_call(num_ret_expected, num_ret_actual)
            };
            self.in_flight.native_call_depth -= 1;
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
        Ok(self.in_flight.stack_bottom + offset)
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
        local_cost: &mut u64,
    ) -> Result<bool> {
        let idx = self.call_base(num_args)?;
        self.publish_cost_unless_lua_callee(idx, local_cost)?;
        // A library leaf runs here without a callback (`vm/leaf.rs`).
        if matches!(self.stack[idx], Val::RustFn(_))
            && let Some(n) = self.call_leaf(idx, num_args)?
        {
            self.drop_surplus_results(num_ret_expected, n);
            return Ok(false);
        }
        let old_stack_bottom = self.in_flight.stack_bottom;
        match crate::native_call!(
            "call_from_lua->begin_call",
            self.begin_call(
                idx,
                num_args,
                old_stack_bottom,
                CallShape::from_lua(method, false),
                ()
            )
        )? {
            Some(n) => {
                self.drop_surplus_results(num_ret_expected, n);
                Ok(false)
            }
            None => Ok(true),
        }
    }

    /// Publish the dispatch loop's pending cost unless the callee at `idx` is
    /// a Lua function. Lua admission never touches the meter, so the batch
    /// carries into the callee; every other callee (a Rust function, a
    /// library leaf, a callable object, a non-callable value's error) sees a
    /// complete meter. A callable object publishes even when its `__call`
    /// resolves to a Lua function: resolution is not inspected here.
    #[inline(always)]
    fn publish_cost_unless_lua_callee(&mut self, idx: usize, local_cost: &mut u64) -> Result<()> {
        let lua = match self.stack[idx] {
            Val::Obj(ptr) => self.heap.as_lua_function_ref(ptr).is_some(),
            _ => false,
        };
        if lua {
            Ok(())
        } else {
            Frame::flush_local_cost(self, local_cost)
        }
    }

    /// The call of the dispatch loop's `OP_TAILCALL`, `return f(...)`: a
    /// proper tail call, which ends the calling activation before the callee
    /// runs, so tail recursion runs at constant call depth and stack. The
    /// caller's upvalues close (its registers are dead, and closing now
    /// captures exactly what an unwind would), the callee and its arguments
    /// slide down into the caller's function slot, and the caller's
    /// `__call` chain units are released. Resolution and admission then run
    /// at that destination as for any call, `__call` receivers and the frame
    /// reservation included, and admission does not count the activation
    /// being replaced against `MAX_CALL_DEPTH`.
    ///
    /// The caller's frame and record stay in place until the callee is
    /// known, for diagnostics only: a refusal is located at the caller's
    /// tail call, its traceback shows the caller, and a Rust callee (which
    /// runs to completion here, as Lua 5.4 runs a C function in tail
    /// position) sees the caller's call site for `print` and host source
    /// context. Nothing reads that frame's registers, closure or upvalues
    /// again: its function slot now holds the callee, so its closure may be
    /// collected while a Rust callee runs, and `stack_bottom` is the
    /// destination slot so the top stays well defined. A Rust callee's
    /// results land at the destination, the caller's function slot, where a
    /// return slides them anyway, and `Some(count)` is returned for the
    /// caller to return them; a Lua callee is admitted in the caller's
    /// place (its record overwrites the caller's), `None` is returned, and
    /// `State::replace_tail_frame` replaces the caller's frame.
    ///
    /// The arm's whole body, from recording the call site (`inst` at
    /// register `ra`, the cursor `ip` past it) on, including publishing the
    /// pending cost for a callee that is not a Lua function: out of line, so the dispatch loop's frame, live
    /// across every native re-entry, holds none of its temporaries. The
    /// result stays register-sized for the same reason: handing back a
    /// whole `Leave` made the loop reserve a separate return slot for it.
    #[inline(never)]
    pub(super) fn tail_call_from_lua(
        &mut self,
        inst: crate::instr::Instr,
        ra: usize,
        ip: usize,
        local_cost: &mut u64,
    ) -> Result<Option<u8>> {
        self.record_call_site(ip);
        let num_args = self.call_window(ra, inst.call_args())?;
        let callee = self.call_base(num_args)?;
        self.publish_cost_unless_lua_callee(callee, local_cost)?;
        let caller_bottom = self.in_flight.stack_bottom;
        let destination = caller_bottom - 1;
        self.close_upvalues(caller_bottom);
        self.stack.drain(destination..callee);
        let caller = self
            .in_flight
            .call_stack
            .last_mut()
            .expect("a tail call is made by a Lua activation");
        let units = caller.chain.units();
        caller.chain = caller.chain.with_units(0);
        self.release_chain_charges(units);
        self.in_flight.stack_bottom = destination;
        // A library leaf runs here, once the caller is retired, with its
        // results at the destination like a Rust callee's (`vm/leaf.rs`).
        if matches!(self.stack[destination], Val::RustFn(_))
            && let Some(n) = self.call_leaf(destination, num_args)?
        {
            return Ok(Some(n));
        }
        let method = inst.call_is_method();
        crate::native_call!(
            "tail_call_from_lua->begin_call",
            self.begin_call(
                destination,
                num_args,
                destination,
                CallShape::from_lua(method, true),
                ()
            )
        )
    }

    /// Finish a tail call whose Lua callee `tail_call_from_lua` admitted:
    /// the admission already overwrote the caller's record with the
    /// callee's, so only the caller's frame is left to replace with the
    /// callee's. Infallible, and nothing between the admission and this can
    /// run code or collect.
    ///
    /// The frame is replaced by truncating and extending in place, never
    /// by moving a whole frame (see `run_invocation`).
    #[inline(never)]
    pub(super) fn replace_tail_frame(&self, frames: &mut Vec<Frame>) {
        frames.truncate(frames.len() - 1);
        frames.extend(std::iter::once_with(|| self.admitted_frame()));
    }

    /// Cut a Rust callee's results to the fixed count an `OP_CALL` expects.
    /// Only the surplus is cut: the arm's resize to the frame top supplies
    /// missing results as nil, inside registers admission reserved (the
    /// verifier keeps fixed results within the frame), so unlike
    /// `finish_call` nothing here can fail the stack cap. A function of its
    /// own so its temporaries stay out of `call_from_lua`'s frame, which is
    /// live across the callback.
    fn drop_surplus_results(&mut self, expected: RetCount, actual: u8) {
        if let RetCount::Fixed(wanted) = expected
            && actual > wanted
        {
            self.stack
                .truncate(self.stack.len() - usize::from(actual - wanted));
        }
    }

    /// Adjust a completed call's results to a fixed count: surplus results
    /// are dropped, missing ones are nil, and only the padding is checked
    /// against the stack cap. Chain owners have already released their
    /// units; any padding error propagates to the boundary or unwind.
    ///
    /// A matching count, the common case, skips the resize entirely. Without
    /// that test the composite `benchmark` workload measured about 2% slower
    /// in same-day interleaved pairs, with `Frame::run` instruction-identical
    /// in both builds; with it the difference went away.
    fn finish_call(&mut self, expected: RetCount, actual: u8) -> Result<()> {
        if let RetCount::Fixed(expected) = expected
            && expected != actual
        {
            self.resize_stack(self.stack.len() - usize::from(actual) + usize::from(expected))?;
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
        shape: CallShape,
        entered: impl CallEntry,
    ) -> Result<Option<u8>> {
        // The two function cases inline into the call sites; only a
        // callable object pays for the resolution loop's setup.
        let func_val = self.stack[idx];
        if let Val::RustFn(f) = func_val {
            // The entry observer is marked inside `invoke_rust_fn`, after its
            // interrupt poll: marking here would report a callback refused
            // before it ran as entered, and a host get_table or set_table
            // would consume operands it promises to keep. A second poll here
            // would not close that, since the flag can be set from another
            // thread between the two.
            //
            // The contiguous Rust ABI needs these arguments relocated: the
            // callback starts at idx and may use every slot up to the global
            // stack cap. Leaving the function at idx and starting at idx + 1
            // would refuse a callback push one slot earlier. Copying results
            // back afterwards does not recover that space during the call.
            // Moving the function elsewhere still shifts the same arguments;
            // a separate result destination adds native-live bookkeeping
            // without removing the relocation required by the stack cap.
            // Keep the single remove (zero copied values for zero arguments).
            //
            // A context ABI that leaves every callback's arguments in place
            // (a retained callee under the same contiguous frame) was weighed
            // and refused for this reason: keeping the cap exact would need a
            // dead-slot credit carried through every nested Rust level. The
            // relocation is only the contiguous `RustFunc` frame's need. A
            // body selected by function identity, like a library leaf, can
            // keep its arguments in place and write its result into the
            // callee slot, running the callback's own stack checks at the
            // same logical lengths (`table.pack` is the case where that saves
            // an argument move per call); such forms belong with the leaves.
            self.stack.remove(idx);
            return crate::native_call!(
                "begin_call->invoke_rust_fn",
                self.invoke_rust_fn(f, idx, shape.method(), entered)
            )
            .map(Some);
        }
        if let Some(bytecode) = self.lua_function_bytecode(func_val) {
            self.admit_lua_call(bytecode, num_args, old_stack_bottom, shape.tail())?;
            entered.mark_entered();
            return Ok(None);
        }
        // Observed calls are only made for direct Lua or Rust __index and
        // __newindex functions (the resolvers hand back nothing else).
        // Callable-object resolution has no host-access observer.
        debug_assert!(!entered.tracks_entry());
        crate::native_call!(
            "begin_call->begin_callable_call",
            self.begin_callable_call(idx, num_args, old_stack_bottom, shape)
        )
    }

    /// `begin_call` for a callee that is not a function: resolve it
    /// (`State::resolve_callable`), then call the Rust handler it resolved
    /// to, holding the chain units the resolution acquired until the
    /// callback returns.
    ///
    /// The resolution is a separate, out-of-line function that has returned
    /// before the callback runs, so its lookups' temporaries are not part of
    /// this frame, which stays live across the callback's re-entry. Inlined
    /// into this function, a call through a Rust-valued handler made without
    /// placing the handler (see `resolve_callable`) grew this frame by 16
    /// bytes in release, the looked-up value's slot apparently kept beside
    /// the callback's result.
    #[inline(never)]
    fn begin_callable_call(
        &mut self,
        idx: usize,
        num_args: u8,
        old_stack_bottom: usize,
        shape: CallShape,
    ) -> Result<Option<u8>> {
        let (rust_handler, charges) =
            match self.resolve_callable(idx, num_args, old_stack_bottom, shape)? {
                Some(resolved) => resolved,
                None => return Ok(None),
            };
        let result = crate::native_call!(
            "begin_callable_call->invoke_rust_fn",
            self.invoke_rust_fn(rust_handler, idx, shape.method(), ())
        )
        .map(Some);
        self.release_chain_charges(charges);
        result
    }

    /// The resolution loop of `begin_callable_call`. A Lua function it
    /// reaches is admitted, the chain units acquired on the way handed to its
    /// record, and `None` is returned. A Rust function is returned with
    /// those units, for the caller to call with the receiver and arguments
    /// from `idx` and release them after. On error the units are released
    /// here.
    ///
    /// Each non-function step puts the `__call` handler in the callee's slot
    /// and inserts the receiver after it, shifting the arguments once per
    /// step. Placing the receivers of a longer non-function chain once is
    /// deliberately not done: a function-valued `__call` is a single step
    /// with a single insert already, and batching the rare multi-step chain
    /// would need the chain kept somewhere, walked twice (repeating the
    /// metatable lookups the moves would save) or reversed and rotated into
    /// place, all while keeping each step's order of type error, argument
    /// overflow, cap check and depth check.
    #[inline(never)]
    fn resolve_callable(
        &mut self,
        idx: usize,
        num_args: u8,
        old_stack_bottom: usize,
        shape: CallShape,
    ) -> Result<Option<(RustFnId, u32)>> {
        let mut num_args = num_args;
        let mut charges: u32 = 0;
        let error = loop {
            // Never a Rust function: `begin_call` calls one directly, and a
            // Rust handler is returned below without being placed here.
            let func_val = self.stack[idx];
            debug_assert!(!matches!(func_val, Val::RustFn(_)));
            if let Some(bytecode) = self.lua_function_bytecode(func_val) {
                // A Lua callee keeps its function slot: the value stays
                // rooted there below the callee's frame, its frame is built
                // from it (`State::admitted_frame`), and the results land in
                // it on return.
                match self.admit_lua_call(bytecode, num_args, old_stack_bottom, shape.tail()) {
                    Ok(()) => {
                        // `with_units` keeps the tail mark a tail admission
                        // set on the record.
                        let record = self
                            .in_flight
                            .call_stack
                            .last_mut()
                            .expect("admission created an activation");
                        record.chain = record.chain.with_units(charges);
                        return Ok(None);
                    }
                    Err(error) => break error,
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
                break self.type_error(TypeError::FunctionCall(func_val.typ(&self.heap)));
            }
            // The receiver becomes the first argument.
            let Some(combined) = num_args.checked_add(1) else {
                break self.error(ErrorKind::TooManyArguments {
                    limit: crate::limits::MAX_ARGUMENTS,
                });
            };
            // The handler takes the callable's slot and the receiver is
            // inserted after it: exactly one slot more than the call began
            // with.
            if let Err(e) = self.check_stack_space(1) {
                break e;
            }
            // A Rust handler would take the slot only to give it up: the
            // callback ABI starts the callee's frame at the first argument,
            // with the function value gone (the direct-function path's slot
            // contract), which is exactly the receiver and the arguments
            // from `idx`, as they already are. So it is called on them
            // unmoved; the cap check above still runs, since the logical call
            // grew by the receiver. A function handler takes no metamethod
            // unit, so nothing below is skipped.
            if let Val::RustFn(f) = call_handler {
                return Ok(Some((f, charges)));
            }
            // Stack is currently: [callable, arg1, ..., argN]
            // We need: [handler, receiver, arg1, ..., argN]
            self.stack[idx] = call_handler;
            self.stack.insert(idx + 1, func_val);
            num_args = combined;
            let handler_is_function = call_handler
                .as_object_ptr()
                .is_some_and(|ptr| self.heap.as_lua_function_ref(ptr).is_some());
            if !handler_is_function {
                #[cfg(not(feature = "fault-injection"))]
                let injected = false;
                #[cfg(feature = "fault-injection")]
                let injected = self.inject_fault(
                    crate::FaultSite::CallChain,
                    self.in_flight.metamethod_depth as usize,
                    true,
                );
                if injected || self.in_flight.metamethod_depth >= MAX_METAMETHOD_DEPTH {
                    break self.error(ErrorKind::MetamethodDepthExceeded {
                        depth: self.in_flight.metamethod_depth,
                        limit: MAX_METAMETHOD_DEPTH,
                    });
                }
                self.in_flight.metamethod_depth += 1;
                charges += 1;
            }
        };
        self.release_chain_charges(charges);
        Err(error)
    }

    /// The single error arm for a call made from Rust. Inner owners have
    /// restored their bookkeeping; this boundary clears the operand suffix
    /// and reports the error before its Rust caller sees it, whether that
    /// caller is the host, a callback that may handle it, or VM code that
    /// propagates it (`report_escaping`).
    #[cold]
    #[inline(never)]
    fn fail_call(&mut self, error: Error, idx: usize, old_stack_bottom: usize) -> Error {
        debug_assert!(self.stack.len() >= idx);
        debug_assert_eq!(self.in_flight.stack_bottom, old_stack_bottom);
        debug_assert!(
            self.in_flight
                .open_upvalues
                .iter()
                .all(|(slot, _)| *slot < idx)
        );
        self.stack.truncate(idx);
        self.in_flight.stack_bottom = old_stack_bottom;
        self.report_escaping(error)
    }

    fn release_chain_charges(&mut self, charges: u32) {
        self.in_flight.metamethod_depth -= charges;
    }

    /// The one place a Rust callback is invoked. Direct calls, handlers
    /// reached through `__call`, and generic-for's Rust iterator fast path all
    /// enter here, so the callback ABI (frame boundary, active function and
    /// result validation) has a single implementation. The interrupt is
    /// polled before callback bookkeeping changes; the enclosing call boundary
    /// or invocation unwind owns error reporting. `entered` is marked right
    /// after that poll succeeds, so an interrupt refusal leaves a host table
    /// access's observer unmarked and its operands preserved; nothing after
    /// the mark can refuse before the callback runs.
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
        function: RustFnId,
        call_base: usize,
        method: bool,
        entered: impl CallEntry,
    ) -> Result<u8> {
        // Every Rust callee passes here, including library functions and
        // metamethods; a library leaf the dispatch loop executes directly is
        // not a callback and takes the same poll in `call_leaf`. Poll before
        // changing callback bookkeeping so a pending interrupt refuses entry
        // and the enclosing boundary owns cleanup.
        // Straight-line Lua can otherwise chain expensive native calls
        // without reaching a backward jump or another Lua admission.
        self.poll_interrupt()?;
        entered.mark_entered();
        let f = self.rust_fns.function(function);
        let old_stack_bottom = self.in_flight.stack_bottom;
        self.in_flight.stack_bottom = call_base;

        // Invariant: the caller's active Rust function and stack_bottom are
        // both restored before a callback error propagates. Every stack index
        // and get_top() after this call is relative to stack_bottom, so the
        // callback result is handled explicitly rather than with `?`.
        let previous_rust_fn = self.in_flight.active_rust_fn.replace(function);
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
        // A callback is host code (and `print` runs `on_print`), so it may
        // return with the thread's floating-point environment changed. Lua
        // must not resume under it: an `Ok` becomes this callback's error,
        // which the arm below unwinds like any other. A callback's own error
        // is kept; nothing executes on the way out of an error. A library
        // entry audited to leave no unchecked host execution behind skips
        // the read (`lua_std::returns_without_host_code`): every host
        // callback it reached was checked at its own return here. `print`
        // is not one of them. The process allocator is assumed to keep the
        // environment, as it is for every allocating opcode.
        let mut result = result;
        if result.is_ok()
            && !crate::lua_std::returns_without_host_code(function)
            && crate::fp_env::unsupported()
        {
            result = Err(Self::float_environment_error());
        }
        self.in_flight.active_rust_fn = previous_rust_fn;
        let num_ret_reported = match result {
            Ok(n) => n,
            Err(e) => {
                self.in_flight.stack_bottom = old_stack_bottom;
                // The argument numbering of an argument error is settled
                // here, by the call that directly invoked the callback.
                return Err(e.settle_method_call(method));
            }
        };

        let available = self.get_top();
        let reported = usize::from(num_ret_reported);
        if reported > available {
            self.in_flight.stack_bottom = old_stack_bottom;
            let error = self.error(ErrorKind::ResultCountExceedsStack {
                reported: num_ret_reported,
                available,
            });
            return Err(error);
        }
        if reported < available {
            // Only the top `reported` values survive. Copy them in order,
            // allowing overlap, rather than rotating discarded arguments and
            // temporaries as well. Val is Copy and discarded slots own no GC
            // resources; truncation below removes the duplicate root slots.
            let kept_start = self.stack.len() - reported;
            self.stack.copy_within(kept_start.., call_base);
            self.stack.truncate(call_base + reported);
        }
        self.in_flight.stack_bottom = old_stack_bottom;
        Ok(num_ret_reported)
    }

    #[cold]
    #[inline(never)]
    fn abandon_poisoned() -> ! {
        std::panic::resume_unwind(Box::new(super::PoisonUnwind))
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
        self.check_usable()?;
        if n < 2 || n > self.get_top() {
            return Err(self.error(ErrorKind::InvalidStackIndex {
                index: isize::try_from(n).map_or(isize::MIN, |n| -n),
            }));
        }
        let idx = self.stack.len() - n;
        let val = self.concat_values(idx, n)?;
        self.stack.truncate(idx);
        self.push_unchecked(val); // Replaces the concatenation operands just removed.
        Ok(())
    }

    /// The concatenation of the `n` stack values from `idx`, read in place.
    /// A rejection leaves them as they were; the caller consumes them.
    /// Outlined: inlined into `OP_CONCAT`'s arm, its rendering and buffer
    /// temporaries grow `Frame::run`, a frame live across every native
    /// re-entry.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    #[inline(never)]
    pub(super) fn concat_values(&mut self, idx: usize, n: usize) -> Result<Val> {
        let operands = idx..idx + n;

        // Capacity hint only: exact string lengths plus the longest number
        // spelling, clamped to the cap. It rejects nothing (an operand of
        // another type contributes 0), and it is at least the length of any
        // accepted result, so the buffer never grows.
        let mut hint = 0usize;
        for val in &self.stack[operands.clone()] {
            if let Some(s) = val.as_string(&self.heap) {
                hint = hint.saturating_add(s.len());
            } else if val.as_num().is_some() {
                hint = hint.saturating_add(crate::number_text::MAX_NUMBER_TEXT);
            }
        }
        let mut buffer = Vec::with_capacity(hint.min(crate::limits::MAX_STRING_BYTES));

        // One ordered pass decides the outcome: each operand is type-checked,
        // numbers are rendered (into inline storage), and the EXACT grown
        // length is checked against the cap before its bytes are copied. So
        // the first offending operand wins, whether by type or by size, and
        // the reported size is the real prefix plus that operand. Numbers are
        // rendered rather than estimated because a fixed estimate is wrong in
        // both directions: over-counting short spellings could reject a legal
        // result - which would also change what the script costs, since the
        // next costed operation never runs - while under-counting long
        // spellings could let the buffer grow past the cap unchecked.
        for val in &self.stack[operands] {
            if let Some(s) = val.as_string(&self.heap) {
                crate::limits::checked_string_growth(buffer.len(), s.len())?;
                buffer.extend_from_slice(s);
            } else if let Some(num) = val.as_num() {
                // The same spelling as tostring and default coercion.
                let text = crate::number_text::number_to_text(num);
                crate::limits::checked_string_growth(buffer.len(), text.as_bytes().len())?;
                buffer.extend_from_slice(text.as_bytes());
            } else {
                return Err(self.type_error(TypeError::Concat(val.typ(&self.heap))));
            }
        }

        // The bytes are owned now, so interning (which may collect, with the
        // operands still rooted where they are) reads nothing from the stack.
        self.alloc_string(&buffer)
    }

    /// Admit a Lua call: check the call depth, push the activation record,
    /// and lay out the callee's frame above the arguments, whose first slot
    /// becomes its stack_bottom. The slot just below holds the callee value,
    /// which keeps it rooted while it runs; the results are returned into
    /// that slot upwards.
    ///
    /// On error nothing stays admitted: the call stack, call depth and
    /// stack_bottom are restored and the frame's slots cleared.
    ///
    /// A `tail` admission replaces the innermost activation, the caller,
    /// instead of adding one, so the call depth does not change and the
    /// caller does not count against the limit. Every refusal comes first,
    /// with the caller's record untouched for its location and traceback;
    /// only then is the record overwritten in place with the callee's,
    /// marked as tail called, taking over the caller's vararg start (the
    /// caller's extra arguments are dropped, the callee's slide down). The
    /// caller's chain units were released by `tail_call_from_lua`. The
    /// dispatch loop replaces the caller's frame (`State::replace_tail_frame`).
    pub(super) fn admit_lua_call(
        &mut self,
        bytecode: Arc<Bytecode>,
        num_args: u8,
        old_stack_bottom: usize,
        tail: bool,
    ) -> Result<()> {
        let depth = self.in_flight.call_depth - u32::from(tail);
        #[cfg(not(feature = "fault-injection"))]
        let injected = false;
        #[cfg(feature = "fault-injection")]
        let injected = self.inject_fault(crate::FaultSite::CallDepth, depth as usize, true);
        if injected || depth >= MAX_CALL_DEPTH {
            return Err(self.error(ErrorKind::CallDepthExceeded {
                depth,
                limit: MAX_CALL_DEPTH,
            }));
        }
        // The callee and arguments are rooted on the stack. Poll here for
        // every Lua entry, including tail calls, since recursion needs no
        // backward jump. An interruption is still reported at the caller's
        // call instruction, before admission changes the frame.
        self.poll_gc_checkpoint()?;
        let frame_bottom = self.stack.len() - num_args as usize;
        self.in_flight.stack_bottom = frame_bottom;

        let num_params = bytecode.num_params;
        let max_stack = usize::from(bytecode.max_stack);
        let is_vararg = bytecode.is_vararg;

        // Extra arguments of a vararg function leave the value stack for the
        // activation record, which roots them.
        // Every admitted activation contributes at most MAX_ARGUMENTS values,
        // and while a tail admission is pending the caller's extra arguments
        // and the callee's are both in the arena: one activation's worth past
        // the depth limit. Prove narrowing safe without adding a fallible
        // admission boundary.
        const {
            assert!((MAX_CALL_DEPTH as u64 + 1) * MAX_ARGUMENTS as u64 <= u32::MAX as u64);
        }
        let vararg_start = self.in_flight.vararg_stack.len() as u32;
        if is_vararg && num_args > num_params {
            let num_varargs = (num_args - num_params) as usize;
            self.collect_varargs(self.stack.len() - num_varargs);
        }

        // A non-vararg function drops its surplus arguments. They are the
        // top `num_args - num_params` of the `num_args` slots `frame_bottom`
        // was measured from, so the cut cannot reach below the frame and
        // needs no check. A vararg function's extra arguments were already
        // collected above.
        if num_args > num_params && !is_vararg {
            self.stack.truncate(frame_bottom + usize::from(num_params));
        }

        // The frame reserves every register it can use, parameters, locals
        // and temporaries, against the stack cap now, and `FRAME_HEADROOM`
        // slots above them for the call window an instruction pushes. No
        // instruction of its own can then fail the cap: every one works in
        // its registers, and the largest window fits. Only a call can still
        // be refused there (the callee's admission, `__call` receiver
        // insertion, a Rust callee's own pushes), and so can growth past
        // the frame by open results (`...`, a call's or return's open
        // count), which that growth checks. Missing arguments and every
        // other register start nil; the headroom is checked, not filled.
        // Compiled code needs only the missing parameters from this fill (it
        // emits LOAD_NIL for every declared local); the rest keeps admitted
        // snapshot bytecode that reads an untouched register reading nil,
        // which no verifier proof replaces. Skipping it would need a
        // definite-initialization dataflow plus a logical height apart from
        // the vector length, and the matching return-side fill, measured at
        // `return_to_caller`, saved nothing.
        let frame_top = frame_bottom + max_stack;
        let grow = (frame_top + FRAME_HEADROOM).saturating_sub(self.stack.len());
        if let Err(e) = self.check_stack_space(grow) {
            return Err(self.reject_admission(e, frame_bottom, old_stack_bottom, vararg_start));
        }
        self.stack.resize(frame_top, Val::Nil);

        if tail {
            self.replace_tail_record(bytecode, vararg_start);
        } else {
            self.in_flight.call_depth += 1;
            // Extended from `once_with` so the record is written straight
            // into its slot; see the frame push in `Frame::eval_batched` for
            // why a plain `push` does not.
            self.in_flight
                .call_stack
                .extend(std::iter::once_with(|| CallInfo {
                    bytecode,
                    ip: 0,
                    chain: ChainUnits::NONE,
                    vararg_start,
                }));
        }
        Ok(())
    }

    /// The commit of a tail admission: drop the caller's extra arguments
    /// from the arena (the callee's, collected above them from
    /// `callee_start`, slide down) and overwrite the caller's record with
    /// the callee's, marked as tail called. Infallible.
    #[inline(never)]
    fn replace_tail_record(&mut self, bytecode: Arc<Bytecode>, callee_start: u32) {
        let caller = self
            .in_flight
            .call_stack
            .last_mut()
            .expect("a tail call is made by a Lua activation");
        debug_assert_eq!(caller.chain.units(), 0);
        let caller_start = caller.vararg_start;
        caller.bytecode = bytecode;
        caller.ip = 0;
        caller.chain = ChainUnits::NONE.marked_tail_called();
        self.in_flight
            .vararg_stack
            .drain(caller_start as usize..callee_start as usize);
    }

    /// The bytecode of `func` if it is a Lua function, without cloning the
    /// rest of its closure.
    fn lua_function_bytecode(&self, func: Val) -> Option<Arc<Bytecode>> {
        match func {
            Val::Obj(ptr) => self
                .heap
                .as_lua_function_ref(ptr)
                .map(|closure| Arc::clone(closure.bytecode())),
            _ => None,
        }
    }

    /// The frame of the activation just admitted, built from the closure in
    /// its function slot.
    pub(super) fn admitted_frame(&self) -> Frame {
        let closure = self.stack[self.in_flight.stack_bottom - 1]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_lua_function_ref(ptr))
            .expect("an admitted Lua activation's function slot holds its closure");
        Frame::new(
            Arc::clone(closure.bytecode()),
            Arc::clone(&closure.runtime),
            Arc::clone(&closure.upvalues),
            self.in_flight.stack_bottom,
        )
    }

    /// Move extra arguments to the shared arena. Admission records the old
    /// length before appending; pop_activation releases exactly that suffix on
    /// return or refusal/unwind. No allocator here triggers VM collection.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    #[inline]
    fn collect_varargs(&mut self, vararg_start: usize) {
        self.in_flight
            .vararg_stack
            .extend_from_slice(&self.stack[vararg_start..]);
        self.stack.truncate(vararg_start);
    }

    /// Undo a refused admission without reporting an unentered callee.
    /// The caller owns the location and traceback at its call instruction.
    /// No record was written yet; the callee's extra arguments, collected
    /// from `vararg_start`, leave the arena.
    #[cold]
    #[inline(never)]
    fn reject_admission(
        &mut self,
        error: Error,
        frame_bottom: usize,
        old_stack_bottom: usize,
        vararg_start: u32,
    ) -> Error {
        self.stack.truncate(frame_bottom);
        self.in_flight.stack_bottom = old_stack_bottom;
        self.in_flight.vararg_stack.truncate(vararg_start as usize);
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
    // Keep the invocation driver in both call_unguarded monomorphizations.
    // Sharing an out-of-line copy adds its frame at every native level;
    // inlining lets its slots overlap the admission and result machinery.
    #[inline(always)]
    pub(super) fn run_invocation(&mut self, old_stack_bottom: usize) -> Result<u8> {
        let mut frames = self.frame_stacks.pop().unwrap_or_default();
        frames.push(self.admitted_frame());
        let result = match crate::native_call!("run_invocation->run", Frame::run(&mut frames, self))
        {
            Ok((first, count)) => {
                let frame = frames.last().expect("the entry frame returned");
                // A host's or callback's expected count is adjusted by
                // `call_unguarded`, whose padding can fail the cap.
                match self.collect_returns(frame, first, count) {
                    Ok(count) => {
                        self.pop_activation();
                        frames.clear();
                        self.in_flight.stack_bottom = old_stack_bottom;
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
    /// release its chain charges, with the results already adjusted to the
    /// caller's `OP_CALL` count. A failure to finish the results belongs to
    /// the returning frame (still on top), and it is the only failure here.
    ///
    /// The caller's pending count is decoded first, so only the results it
    /// keeps move: surplus ones are still validated against `MAX_RESULTS`
    /// before they are dropped, but never slid. Missing ones are the nil
    /// fill up to the caller's frame top, which cannot fail the stack cap:
    /// admission verified that `OP_CALL`'s fixed results lie inside the
    /// caller's registers, which it reserved when it was admitted.
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
        let len = frames.len();
        // The caller published its cursor when it suspended at its `OP_CALL`.
        let expected = frames[len - 2].pending_call_results();
        let keep = match expected {
            RetCount::Fixed(wanted) => count.min(usize::from(wanted)),
            RetCount::All => count,
        };
        self.collect_kept_returns(&frames[len - 1], first, count, keep)?;
        self.pop_activation();
        frames.truncate(len - 1);
        let caller = frames.last().expect("a nested return has a Lua caller");
        self.in_flight.stack_bottom = caller.stack_bottom;
        // A fixed result count resumes ordinary execution at the caller's
        // frame top, the missing results and every register above them nil;
        // all results stay open for the consuming instruction.
        //
        // Restoring only what the code after the call needs was built and
        // measured, and refused. A per-function table keyed by resume pc gave
        // the length the straight-line run up to the next fixed call or
        // return names (any branch, handler-capable instruction, CLOSURE,
        // VARARG or open consumer fell back to the frame top, since a window
        // opened lower would move the stack cap its callee sees). The fill
        // grows with the caller's register high-water mark, quadratically in
        // a constructor of call results, so a 128-field `{f(i), ...}` was
        // the best case: about 8,000 nil stores saved per construction. On
        // bygg against de60b60 the same binary with the table filled with
        // full tops versus short ones ran the host-entered warm calls 54.9
        // against 61.5 ms and the Lua-entered setup 1.62 against 1.50 s:
        // offset-sensitive swings either way, no net saving. The fill is
        // independent tag stores that the store buffer absorbs behind the
        // call machinery, so its store count is not its cost.
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
                self.in_flight.stack_bottom = caller.stack_bottom;
            }
        }
        self.in_flight.stack_bottom = old_stack_bottom;
        error
    }

    /// Pop the innermost activation record and its varargs, releasing the
    /// `__call` chain charges the record held.
    ///
    /// The release comes first, and stays inside: the record owns the
    /// units, so popping it is what releases them, and a count returned for
    /// the caller to release would be live across the record's drop (an
    /// `Arc` decrement) in a callee-saved register.
    /// In `run_invocation`, inlined into `call_unguarded`, that one extra
    /// live value spilled `old_stack_bottom` and split the result slot
    /// `Frame::run` and `begin_callable_call` had shared, growing a frame
    /// that is live across every native re-entry by 32 bytes in release.
    fn pop_activation(&mut self) {
        let len = self.in_flight.call_stack.len();
        let info = &self.in_flight.call_stack[len - 1];
        self.in_flight.metamethod_depth -= info.chain.units();
        self.in_flight
            .vararg_stack
            .truncate(info.vararg_start as usize);
        self.in_flight.call_stack.truncate(len - 1);
        self.in_flight.call_depth -= 1;
    }

    /// Finish a returning frame's results, the `count` values from register
    /// index `first`: close the frame's upvalues, and slide the first `keep`
    /// of them (`keep <= count`) down into its function slot, just below its
    /// stack_bottom, and upwards; the rest are dropped unmoved. Fails, before
    /// changing anything, only when `count`, the whole count, exceeds
    /// `MAX_RESULTS`; that error belongs to the returning frame.
    ///
    /// The entry frame keeps them all (`collect_returns`): a separate entry
    /// point, because passing the extra count from `run_invocation`, inlined
    /// into `call_unguarded`, grew that native-live frame in the dev build.
    fn collect_kept_returns(
        &mut self,
        frame: &Frame,
        first: usize,
        count: usize,
        keep: usize,
    ) -> Result<u8> {
        debug_assert!(keep <= count);
        let returned = crate::error::results_count(count).map_err(|kind| self.error(kind))?;

        // Close open upvalues BEFORE moving or dropping anything: results can
        // come straight from locals, and every captured local must still hold
        // its value when the close reads it.
        self.close_upvalues(frame.stack_bottom);

        // Drop the registers above the kept results, then slide them down
        // over the frame in one memmove: drain the function slot and every
        // register beneath them and let the tail shift. The stack only ever
        // shrinks here, so the cap needs no preflight.
        self.stack.truncate(first + keep);
        self.stack.drain(frame.stack_bottom - 1..first);
        Ok(returned)
    }

    /// `collect_kept_returns` keeping every result: the entry frame's
    /// return, adjusted afterwards by its Rust caller.
    fn collect_returns(&mut self, frame: &Frame, first: usize, count: usize) -> Result<u8> {
        self.collect_kept_returns(frame, first, count, count)
    }

    /// Locate and report an error leaving the Lua frame it surfaced in.
    #[cold]
    #[inline(never)]
    fn report_frame_error(&mut self, error: Error, frame: &Frame) -> Error {
        let mut error = self.locate_error(error, frame.bytecode(), frame.cursor());
        self.host_error(&mut error);
        error
    }

    /// Report an execution error at the point it is returned to Rust code
    /// that may handle it rather than propagate it: a call made from Rust
    /// (`fail_call`), a host table read or a `tostring` rendering. Errors a
    /// Lua frame already reported pass through unchanged. Otherwise the error
    /// is first located exactly as the next Lua frame unwind would locate it:
    /// that frame is the innermost activation, suspended at the call site its
    /// record holds (every dispatch path that can call out records it first,
    /// see `State::record_call_site`), and no Rust frame in between alters
    /// the error. Propagating it there then finds a reported error with a
    /// trace and changes nothing, so the error and the one notification are
    /// the same whether a callback handles it or returns it. With no Lua
    /// activation (a host-initiated call) there is nothing to locate.
    #[cold]
    #[inline(never)]
    pub(super) fn report_escaping(&mut self, mut error: Error) -> Error {
        if error.is_reported() {
            return error;
        }
        if let Some(info) = self.in_flight.call_stack.last() {
            error = self.locate_error(error, &info.bytecode, info.ip);
        }
        self.host_error(&mut error);
        error
    }

    /// Fill in what an error lacks from the innermost activation, whose code
    /// is `bytecode` at `cursor`: the innermost fault, then, for an error
    /// with no traceback yet, the traceback and the source line. An error
    /// that already carries a line keeps it, and one whose location the
    /// compiler sealed keeps its line, column and source name as they are,
    /// absent ones included, so `Display` names the rejected source rather
    /// than the caller (`Error::seal_location`); one that carries a
    /// traceback (a nested frame's, or a host-supplied one) keeps both.
    /// Allocates metadata only here, outside the dispatch loop.
    fn locate_error(&self, mut error: Error, bytecode: &Arc<Bytecode>, cursor: usize) -> Error {
        // Preserve the innermost fault even when a callback already attached
        // a traceback.
        error = Self::locate_fault(error, bytecode, cursor);
        if error.stack_trace().is_empty() {
            let trace = self.build_stack_trace(bytecode, cursor);
            if error.has_sealed_location() {
                // A compile error returned through a callback keeps the
                // rejected source's location, absence included; only the
                // traceback is the caller's.
            } else if Self::is_gmatch_wrapper(bytecode) {
                if let Some(caller) = trace.first() {
                    if error.line_num() == 0 {
                        let column = error.column();
                        error = error.with_location(caller.line, column);
                    }
                    error = error.with_source_name(caller.source.clone());
                }
            } else if error.line_num() == 0 {
                let column = error.column();
                error = error.with_location(Self::line_before(bytecode, cursor), column);
            }
            error = error.with_stack_trace(trace);
        }
        error
    }

    #[cold]
    #[inline(never)]
    fn locate_fault(error: Error, bytecode: &Bytecode, cursor: usize) -> Error {
        let Some(instruction) = cursor.checked_sub(1) else {
            return error;
        };
        let Some(inst) = bytecode.code.get(instruction) else {
            return error;
        };
        error.with_vm_fault(instruction, inst.opcode(), bytecode.name.as_ref())
    }

    /// Push a closure of `bytecode` with no captures.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(crate) fn push_chunk(&mut self, bytecode: &Arc<Bytecode>) -> Result<()> {
        self.check_stack_space(1)?;
        let runtime = self.resolve_bytecode_runtime(bytecode)?;
        let upvalues = self.heap.empty_upvalues();
        let obj = self.alloc_closure(runtime, upvalues);
        self.push_unchecked(Val::Obj(obj));
        Ok(())
    }

    /// Allocate a closure with its resolved `runtime`. The caller roots the
    /// result before anything else can collect.
    pub(super) fn alloc_closure(
        &mut self,
        runtime: Arc<super::BytecodeRuntime>,
        upvalues: Arc<[UpvalueRef]>,
    ) -> super::object::ObjectPtr {
        let identity = Arc::as_ptr(&runtime.bytecode) as usize;
        // No host callback or fallible operation occurs from this push through
        // its matching pop. The allocator's collection check must see the
        // pending identity so its cache sweep retains the runtime before the
        // closure becomes a root; the allocation itself never collects.
        // Nested captures finish beforehand.
        self.in_flight.pending_bytecode_caches.push(identity);
        let runtime_ptr = Arc::as_ptr(&runtime);
        let obj = self.allocator().alloc_lua_fn(runtime, upvalues);
        // A collection inside `allocator` must have kept this runtime as the
        // cached one, or the weak cache processing would no longer reach it.
        debug_assert!(
            self.bytecode_caches
                .get(&identity)
                .is_some_and(|runtime| std::ptr::eq(Arc::as_ptr(runtime), runtime_ptr))
        );
        let pending = self
            .in_flight
            .pending_bytecode_caches
            .pop()
            .expect("pending bytecode cache missing after closure allocation");
        debug_assert_eq!(pending, identity);
        obj
    }

    /// Find an existing open upvalue for the given stack index, or create a new one.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(super) fn find_or_create_upvalue(&mut self, stack_idx: usize) -> UpvalueRef {
        // Binary search preserves identity on hits and the exact ascending
        // insertion/descending close order on misses. Vec insertion still
        // shifts entries, but changing the close order would change pool reuse.
        // The shift is bounded by the frame width, not by call depth: only the
        // executing frame captures new locals, every frame exit closes what it
        // opened, so entries above the insertion point are this frame's own
        // (at most MAX_FRAME_SLOTS - 1 of them).
        let pos = match self
            .in_flight
            .open_upvalues
            .binary_search_by_key(&stack_idx, |(idx, _)| *idx)
        {
            Ok(pos) => return self.in_flight.open_upvalues[pos].1,
            Err(pos) => pos,
        };
        let uv_ref = self.upvalue_pool.alloc(Upvalue::Open(stack_idx));
        self.in_flight
            .open_upvalues
            .insert(pos, (stack_idx, uv_ref));
        uv_ref
    }

    /// Close all open upvalues at or above the given stack level.
    /// This is called when a function returns to capture the values from the stack
    /// before they are popped.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(crate) fn close_upvalues(&mut self, level: usize) {
        // Upvalues are sorted ascending by stack index, so highest indices are at the end.
        // We close highest first (they go out of scope first), popping from end in O(1).
        while let Some(&(idx, _)) = self.in_flight.open_upvalues.last() {
            if idx < level {
                break;
            }
            let (_, uv_ref) = self
                .in_flight
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
        self.push_chunk(&bytecode)?;
        let func = self.pop_val();
        self.stack.insert(func_slot, func);
        let old_stack_bottom = self.in_flight.stack_bottom;
        self.admit_lua_call(bytecode, num_args, old_stack_bottom, false)?;
        self.run_invocation(old_stack_bottom)
    }
}
