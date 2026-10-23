use std::cell::Cell;
use std::sync::Arc;

// The constant folder and runtime dispatch use the arithmetic functions
// declared with each opcode in `instr.rs`.
use crate::numeral::{lua_add, lua_div, lua_modulo, lua_mul, lua_sub};

use super::super::error::{Error, StackFrame};
use super::Bytecode;
use super::BytecodeRuntime;
use super::Instr;
use super::Result;
use super::State;
use super::Val;
use super::object::UpvalueRef;
use crate::instr::{ArgCount, CompareOp, RetCount};
use crate::limits::tuning;

/// A `Frame` represents a single stack-frame of a Lua function.
///
/// The frame's registers are the stack slots `[stack_bottom, top)`, reserved
/// and nil-filled at admission. While its bytecode runs the stack is at least
/// `top` long; it is exactly `top` except between an instruction that leaves
/// open results and the one that consumes them, and while an instruction's
/// own operands for a callback sit above the frame.
pub(super) struct Frame {
    /// The bytecode being executed (shared via Arc; cheap to clone, immutable).
    bytecode: Arc<Bytecode>,
    /// State-local literals and lookup caches shared by this bytecode's
    /// closures and active frames.
    pub(super) runtime: Arc<BytecodeRuntime>,
    /// The index of the next (not current) instruction. While the frame runs,
    /// the dispatch loop's local cursor is authoritative and this copy is
    /// published only where something can read it: before a call suspends
    /// the frame, before a return, and on every error escape. A `Cell`, so
    /// the loop can hold the frame by shared reference alongside its code.
    ip: Cell<usize>,
    /// The upvalues captured by this closure. Shared with the `Closure`
    /// (and any other active frames on it) - never mutated through the
    /// frame; writes go through the `UpvaluePool` slots.
    pub(super) upvalues: Arc<[UpvalueRef]>,
    /// The frame's first register (used for closing upvalues).
    pub(super) stack_bottom: usize,
    /// One past the frame's last register.
    pub(super) top: usize,
}

impl Frame {
    /// Create a frame from bytecode admitted by compilation or snapshot load.
    /// No verification is repeated here: this runs on every call.
    #[must_use]
    pub(super) fn new(
        bytecode: Arc<Bytecode>,
        runtime: Arc<BytecodeRuntime>,
        upvalues: Arc<[UpvalueRef]>,
        stack_bottom: usize,
    ) -> Self {
        let top = stack_bottom + usize::from(bytecode.max_stack);
        Self {
            bytecode,
            runtime,
            ip: Cell::new(0),
            upvalues,
            stack_bottom,
            top,
        }
    }

    /// Get the bytecode being executed.
    pub(super) fn bytecode(&self) -> &Arc<Bytecode> {
        &self.bytecode
    }

    pub(super) fn literal(&self, i: u16) -> Val {
        self.runtime.literals[i as usize]
    }

    #[cold]
    #[inline(never)]
    pub(super) fn locate_fault(&self, error: Error) -> Error {
        let Some(instruction) = self.ip.get().checked_sub(1) else {
            return error;
        };
        let Some(inst) = self.bytecode.code.get(instruction) else {
            return error;
        };
        error.with_vm_fault(instruction, inst.opcode(), self.bytecode.name.as_ref())
    }

    /// Get the current line number (1-indexed), or 0 if unknown.
    pub(super) fn current_line(&self) -> u32 {
        // ip points to the NEXT instruction, so use ip-1 for current
        let idx = self.ip.get().saturating_sub(1);
        self.bytecode.line_info.get(idx).copied().unwrap_or(0)
    }

    /// Create a StackFrame for error reporting.
    pub(super) fn to_stack_frame(&self) -> StackFrame {
        StackFrame {
            function_name: self.bytecode.name.clone(),
            source: self.bytecode.source.clone(),
            line: self.current_line(),
            defined_line: self.bytecode.defined_line,
        }
    }

    // Static bytecode contracts are checked at compile/load admission, not
    // again during dispatch. Like literal and register indexing, a verifier
    // regression here is a VM bug and may panic. Runtime values, host API
    // inputs and resource limits still require fallible checks.
    /// The cursor `ip` moved forward/back by a verified `offset` in
    /// instructions.
    #[inline]
    pub(super) fn jumped(ip: usize, offset: i32) -> usize {
        if offset >= 0 {
            ip + offset as usize
        } else {
            ip - offset.unsigned_abs() as usize
        }
    }

    #[must_use]
    pub(super) fn get_nested_bytecode(&self, i: u16) -> Arc<Bytecode> {
        Arc::clone(&self.bytecode.nested[i as usize])
    }

    #[must_use]
    pub(super) fn get_number_constant(&self, i: u16) -> f64 {
        self.bytecode.number_literals[i as usize]
    }

    /// Settle this frame's accrued cost before control can re-enter Lua. Table
    /// helpers call this only after a direct access path has been ruled out.
    pub(super) fn flush_local_cost(state: &mut State, local_cost: &mut u64) -> Result<()> {
        if *local_cost > 0 {
            state.charge_cost(*local_cost)?;
            *local_cost = 0;
        }
        Ok(())
    }

    /// Charge a full cost batch. Outlined: every costed opcode expands the
    /// batching check, and the charge itself runs once per batch.
    #[cold]
    #[inline(never)]
    fn settle_cost(state: &mut State, local_cost: &mut u64, next: u64) -> Result<()> {
        state.charge_cost(next)?;
        *local_cost = 0;
        Ok(())
    }

    /// The `RetCount` operand of the `OP_CALL` this suspended frame is
    /// waiting on. A suspended frame's `ip` points just past that call, and
    /// its bytecode is immutable, so the instruction can be read back rather
    /// than stored per activation.
    pub(super) fn pending_call_results(&self) -> RetCount {
        self.bytecode.code[self.ip.get() - 1].call_results()
    }

    /// Run one dispatch-loop invocation over `frames`, which holds its entry
    /// frame. Lua calls push the callee's frame and continue in it; returns
    /// pop back to the caller; neither recurses natively. Returns when the
    /// entry frame returns, with the absolute index of its first result and
    /// its result count, or with an error, leaving `frames` as it was at the
    /// failing instruction for the caller to unwind
    /// (`State::unwind_invocation`).
    ///
    /// Fixed opcode charges come from `Instr::fixed_units`. SET_LIST charges
    /// its live element count (the analyzer only knows its encoded count).
    /// Calls and access helpers can additionally charge native work, including
    /// strings, patterns, table traversal and metamethod callbacks.
    pub(super) fn run(frames: &mut Vec<Frame>, state: &mut State) -> Result<(usize, usize)> {
        // Batch cost checking: accumulate locally and flush periodically.
        // Calls and returns flush before leaving, so a batch never outlives
        // the run of the frame that accrued it.
        let mut pending: u64 = 0;
        // native-call: run->eval_batched
        let result = Self::eval_batched(frames, state, &mut pending);
        if result.is_err() && pending > 0 {
            // An error leaving the dispatch loop must still charge the work
            // that already executed, or `cost_used` after a failed callback
            // would depend on the batching interval rather than on the
            // script. Pending cost never reaches the budget edge (add_cost!
            // flushes first when it would), so for any error other than
            // BudgetExceeded this charge is admitted. For BudgetExceeded the
            // meter is already at or below zero and refuses it without
            // mutating anything, which keeps that path's `cost_used`
            // unchanged. Either way the original error is what propagates.
            let _admitted = state.cost_meter().consume(pending);
        }
        result
    }

    /// Dispatch loop body for [`Frame::run`]. `local_cost` holds accrued but
    /// not yet flushed opcode cost; the caller settles it on error.
    #[inline(always)]
    fn eval_batched(
        frames: &mut Vec<Frame>,
        state: &mut State,
        local_cost: &mut u64,
    ) -> Result<(usize, usize)> {
        // Lua calls and returns switch to another entry of `frames` and
        // continue here: no native recursion and no re-entry of this function
        // per call. Frames never move while they run; a pushed frame is
        // written straight into its slot (`extend` from `once_with`), because
        // a whole-frame move reads back fields the loop wrote one at a time
        // with wide loads that cannot be store-forwarded.
        loop {
            let frame: &Frame = frames.last().expect("a running invocation has a frame");
            // The authoritative cursor while the frame runs. Every `Result`
            // exit from `run_frame`, a call, a return or an error (a panic
            // poisons the State instead), publishes it to
            // `Frame::ip` here, before anything can read that: the unwind
            // locates a fault from it, a returning callee decodes the caller's
            // pending call from it, and `return_to_caller` may pop the frame.
            let mut ip = frame.ip.get();
            // native-call: eval_batched->run_frame
            let leave = Self::run_frame(frame, state, local_cost, &mut ip);
            frame.ip.set(ip);
            match leave? {
                Leave::Call => frames.extend(std::iter::once_with(|| state.admitted_frame())),
                Leave::Return { first, count } => {
                    if frames.len() == 1 {
                        // The entry frame: its Rust caller finishes the call.
                        return Ok((first, count));
                    }
                    state.return_to_caller(frames, first, count)?;
                }
            }
        }
    }

    /// Dispatch one activation of `frame` from the cursor `ip` until it calls
    /// a Lua function (admitted, its frame for the caller to push) or
    /// returns. Inlined, so `ip` stays a register for the whole run; the
    /// caller publishes it on every exit, which is why nothing here writes
    /// `Frame::ip` and every error simply propagates.
    #[inline(always)]
    fn run_frame(
        frame: &Frame,
        state: &mut State,
        local_cost: &mut u64,
        ip: &mut usize,
    ) -> Result<Leave> {
        let code: &[Instr] = &frame.bytecode.code;
        let base = frame.stack_bottom;

        /// Macro to accumulate cost and flush when threshold is reached
        macro_rules! add_cost {
            ($cost:expr) => {{
                let next = (*local_cost).saturating_add($cost);
                // This decides when to flush, not whether to accept a charge.
                // consume_cost owns the runtime meter's admission rule. A
                // finite budget also flushes before pending cost would exhaust
                // it, so the interval does not change the charged prefix or
                // `cost_used` at `BudgetExceeded`; any other error settles the
                // pending batch in `Frame::run`.
                if next >= tuning::COST_CHECK_INTERVAL || state.cost_batch_needs_flush(next) {
                    Self::settle_cost(state, local_cost, next)?;
                } else {
                    *local_cost = next;
                }
            }};
        }

        // One arm per arithmetic opcode, each a direct call to its function
        // item with its operand kinds fixed.
        macro_rules! arith {
            ($inst:ident, $f:path, rr) => {{
                add_cost!(1);
                let lhs = state.stack[base + $inst.b() as usize];
                let rhs = state.stack[base + $inst.c() as usize];
                match (lhs, rhs) {
                    (Val::Num(x), Val::Num(y)) => {
                        state.stack[base + $inst.a() as usize] = Val::Num($f(x, y));
                    }
                    _ => return Err(state.arithmetic_operands_error(lhs, rhs)),
                }
            }};
            ($inst:ident, $f:path, rk) => {{
                add_cost!(1);
                let lhs = state.stack[base + $inst.b() as usize];
                let y = frame.get_number_constant($inst.c());
                match lhs {
                    Val::Num(x) => state.stack[base + $inst.a() as usize] = Val::Num($f(x, y)),
                    _ => return Err(state.arithmetic_operands_error(lhs, Val::Num(y))),
                }
            }};
            ($inst:ident, $f:path, kr) => {{
                add_cost!(1);
                let x = frame.get_number_constant($inst.b());
                let rhs = state.stack[base + $inst.c() as usize];
                match rhs {
                    Val::Num(y) => state.stack[base + $inst.a() as usize] = Val::Num($f(x, y)),
                    _ => return Err(state.arithmetic_operands_error(Val::Num(x), rhs)),
                }
            }};
        }

        loop {
            let inst = code[*ip];
            *ip += 1;
            let ra = base + inst.a() as usize;
            match inst.opcode() {
                // Moves and constants
                Instr::OP_MOVE => {
                    state.stack[ra] = state.stack[base + inst.b() as usize];
                }
                Instr::OP_LOAD_NIL => {
                    state.stack[ra..ra + inst.b() as usize].fill(Val::Nil);
                }
                Instr::OP_LOAD_BOOL => state.stack[ra] = Val::Bool(inst.b() != 0),
                Instr::OP_LOAD_NUM => {
                    state.stack[ra] = Val::Num(frame.get_number_constant(inst.b()));
                }
                Instr::OP_LOAD_STR => state.stack[ra] = frame.literal(inst.b()),

                // Upvalues
                Instr::OP_GET_UPVAL => {
                    state.stack[ra] = state.upvalue_value(frame, inst.b());
                }
                Instr::OP_SET_UPVAL => {
                    let val = state.stack[ra];
                    state.set_upvalue_value(frame, inst.b(), val);
                }
                Instr::OP_CLOSE => state.close_upvalues(ra),

                // Globals and builtins
                Instr::OP_GET_GLOBAL => {
                    state.stack[ra] = state.read_global(frame, inst.b(), inst.cache_slot());
                }
                Instr::OP_SET_GLOBAL => {
                    let val = state.stack[ra];
                    state.write_global(frame, inst.b(), inst.cache_slot(), val);
                }
                Instr::OP_GET_BUILTIN => {
                    state.stack[ra] = state.global_env.builtin(inst.b() as u8);
                }
                Instr::OP_SET_BUILTIN => {
                    let val = state.stack[ra];
                    state.global_env.write_builtin(inst.b() as u8, val);
                }

                // Table creation
                Instr::OP_NEW_TABLE => {
                    add_cost!(const { Instr::fixed_units(Instr::OP_NEW_TABLE) });
                    state.stack[ra] = state.alloc_table();
                }
                Instr::OP_NEW_TABLE_PRESIZED => {
                    add_cost!(const { Instr::fixed_units(Instr::OP_NEW_TABLE_PRESIZED) });
                    // B is a hash-capacity hint: the constructor's keyed-field
                    // count. The array part is reserved per SET_LIST batch.
                    state.stack[ra] = state.alloc_table_with_capacity(inst.b() as usize);
                }
                Instr::OP_NEW_TABLE_TEMPLATE => {
                    add_cost!(const { Instr::fixed_units(Instr::OP_NEW_TABLE_TEMPLATE) });
                    state.reg_new_table_template(frame, ra, inst.b())?;
                }

                // Table reads
                // Table reads. These and the table writes and length below
                // pass the cursor down: their handlers record it as the call
                // site only on the path that can call out
                // (`State::record_call_site`).
                Instr::OP_GET_FIELD => {
                    let receiver = state.stack[base + inst.b() as usize];
                    // native-call: run_frame->reg_get_field:field
                    state.reg_get_field(frame, inst, ra, receiver, local_cost, *ip)?;
                }
                Instr::OP_SELF => {
                    let receiver = state.stack[base + inst.b() as usize];
                    state.stack[ra + 1] = receiver;
                    // native-call: run_frame->reg_get_field:self
                    state.reg_get_field(frame, inst, ra, receiver, local_cost, *ip)?;
                }
                Instr::OP_GET_TABLE => {
                    // native-call: run_frame->reg_get_table
                    state.reg_get_table(
                        ra,
                        base + inst.b() as usize,
                        base + inst.c() as usize,
                        local_cost,
                        *ip,
                    )?;
                }

                // Table writes
                Instr::OP_SET_FIELD => {
                    add_cost!(const { Instr::fixed_units(Instr::OP_SET_FIELD) });
                    // native-call: run_frame->reg_set_field
                    state.reg_set_field(
                        frame,
                        inst,
                        ra,
                        base + inst.c() as usize,
                        local_cost,
                        *ip,
                    )?;
                }
                Instr::OP_SET_TABLE => {
                    add_cost!(const { Instr::fixed_units(Instr::OP_SET_TABLE) });
                    // native-call: run_frame->reg_set_table
                    state.reg_set_table(
                        ra,
                        base + inst.b() as usize,
                        base + inst.c() as usize,
                        local_cost,
                        *ip,
                    )?;
                }
                Instr::OP_INIT_FIELD => {
                    add_cost!(const { Instr::fixed_units(Instr::OP_INIT_FIELD) });
                    state.reg_init_field(frame, ra, inst.b(), base + inst.c() as usize)?;
                }
                Instr::OP_INIT_FIELD_PINNED => {
                    add_cost!(const { Instr::fixed_units(Instr::OP_INIT_FIELD_PINNED) });
                    state.reg_init_field_pinned(
                        frame,
                        ra,
                        inst.b(),
                        base + inst.c() as usize,
                        inst.d(),
                    )?;
                }
                Instr::OP_INIT_INDEX => {
                    add_cost!(const { Instr::fixed_units(Instr::OP_INIT_INDEX) });
                    state.reg_init_index(ra, base + inst.b() as usize, base + inst.c() as usize)?;
                }
                Instr::OP_SET_LIST => {
                    let count = match inst.set_list_count() {
                        Some(count) => usize::from(count),
                        None => state.stack.len() - (ra + 1),
                    };
                    add_cost!(count as u64);
                    state.reg_set_list(ra, count, inst.c())?;
                    if inst.set_list_count().is_none() {
                        state.stack.resize(frame.top, Val::Nil);
                    }
                }

                // Arithmetic
                Instr::OP_ADD_RR => arith!(inst, lua_add, rr),
                Instr::OP_ADD_RK => arith!(inst, lua_add, rk),
                Instr::OP_ADD_KR => arith!(inst, lua_add, kr),
                Instr::OP_SUB_RR => arith!(inst, lua_sub, rr),
                Instr::OP_SUB_RK => arith!(inst, lua_sub, rk),
                Instr::OP_SUB_KR => arith!(inst, lua_sub, kr),
                Instr::OP_MUL_RR => arith!(inst, lua_mul, rr),
                Instr::OP_MUL_RK => arith!(inst, lua_mul, rk),
                Instr::OP_MUL_KR => arith!(inst, lua_mul, kr),
                Instr::OP_DIV_RR => arith!(inst, lua_div, rr),
                Instr::OP_DIV_RK => arith!(inst, lua_div, rk),
                Instr::OP_DIV_KR => arith!(inst, lua_div, kr),
                Instr::OP_MOD_RR => arith!(inst, lua_modulo, rr),
                Instr::OP_MOD_RK => arith!(inst, lua_modulo, rk),
                Instr::OP_MOD_KR => arith!(inst, lua_modulo, kr),
                Instr::OP_POW_RR => arith!(inst, crate::crmath::pow, rr),
                Instr::OP_POW_RK => arith!(inst, crate::crmath::pow, rk),
                Instr::OP_POW_KR => arith!(inst, crate::crmath::pow, kr),
                Instr::OP_UNM => {
                    add_cost!(const { Instr::fixed_units(Instr::OP_UNM) });
                    match state.stack[base + inst.b() as usize] {
                        Val::Num(n) => state.stack[ra] = Val::Num(-n),
                        other => return Err(state.arithmetic_operands_error(other, other)),
                    }
                }
                Instr::OP_NOT => {
                    state.stack[ra] = Val::Bool(!state.stack[base + inst.b() as usize].truthy());
                }
                Instr::OP_LEN => {
                    // native-call: run_frame->reg_len
                    state.reg_len(ra, base + inst.b() as usize, local_cost, *ip)?;
                }

                // The bytes `..` emits are deliberately uncharged: the
                // budget is a game-design instrument rather than a CPU
                // meter (docs/budget.md), and `..` is structural syntax
                // that a legible refactor must not make more expensive.
                // The size is still bounded by `checked_string_growth`
                // (MAX_STRING_BYTES), so the uncharged work is capped.
                Instr::OP_CONCAT => {
                    state.reg_concat(ra, base + inst.b() as usize, inst.c() as usize)?;
                }

                // Comparisons
                Instr::OP_LT..=Instr::OP_NE => {
                    let op = CompareOp::from_value_opcode(inst.opcode())
                        .expect("the range holds the value comparisons");
                    let lhs = state.stack[base + inst.b() as usize];
                    let rhs = state.stack[base + inst.c() as usize];
                    state.stack[ra] = Val::Bool(state.compare_vals(op, lhs, rhs)?);
                }

                // Control flow
                Instr::OP_JMP => *ip = Self::jumped(*ip, inst.sj()),
                // Compare-and-branch: jump when the comparison is false.
                // One range arm with the variant decode outlined - per-
                // variant arms measurably perturbed the dispatch loop's
                // codegen in the stack VM.
                Instr::OP_BR_LT..=Instr::OP_BR_NE => {
                    let op = CompareOp::from_branch_opcode(inst.opcode())
                        .expect("the range holds the branch comparisons");
                    let lhs = state.stack[ra];
                    let rhs = state.stack[base + inst.b() as usize];
                    if !state.compare_vals(op, lhs, rhs)? {
                        *ip = Self::jumped(*ip, inst.sj());
                    }
                }
                Instr::OP_TEST_FALSE => {
                    if !state.stack[ra].truthy() {
                        *ip = Self::jumped(*ip, inst.sj());
                    }
                }
                Instr::OP_TEST_TRUE => {
                    if state.stack[ra].truthy() {
                        *ip = Self::jumped(*ip, inst.sj());
                    }
                }

                // `for` loops
                Instr::OP_FOR_PREP => state.instr_for_prep(ra, ip, inst.sj())?,
                Instr::OP_FOR_LOOP => state.instr_for_loop(ra, ip, inst.sj())?,
                Instr::OP_TFOR_CALL => {
                    // next/ipairs need no record or flush, but deferring this
                    // boundary into the iterator helper, or behind a separate
                    // fast-path probe here, grew the release Frame::run (see
                    // reference/native-calls.md, "Shared vararg arena
                    // follow-up"). Removing the fallback's redundant error
                    // truncate did not help. Keep the eager boundary rather
                    // than grow a frame live across every native re-entry.
                    // All call/refusal paths must record and flush before
                    // entering the iterator.
                    state.record_call_site(*ip);
                    Self::flush_local_cost(state, local_cost)?;
                    // native-call: run_frame->instr_tfor_call
                    state.instr_tfor_call(
                        ra,
                        inst.b() as u8,
                        inst.cache_slot(),
                        &frame.runtime.caches,
                    )?;
                }
                Instr::OP_TFOR_LOOP => state.instr_tfor_loop(ra, ip, inst.sj()),

                // Functions (calls and returns)
                Instr::OP_CLOSURE => state.reg_closure(frame, ra, inst.b())?,
                Instr::OP_CALL => {
                    state.record_call_site(*ip);
                    Self::flush_local_cost(state, local_cost)?;
                    let args = state.call_window(ra, inst.call_args())?;
                    let results = inst.call_results();
                    // native-call: run_frame->call_from_lua
                    if state.call_from_lua(args, results, inst.call_is_method())? {
                        return Ok(Leave::Call);
                    }
                    if let RetCount::Fixed(_) = results {
                        state.stack.resize(frame.top, Val::Nil);
                    }
                }
                Instr::OP_RETURN => {
                    // Flush any remaining accumulated cost before returning
                    Self::flush_local_cost(state, local_cost)?;
                    let count = match inst.return_count() {
                        RetCount::Fixed(n) => usize::from(n),
                        RetCount::All => state.stack.len() - ra,
                    };
                    return Ok(Leave::Return { first: ra, count });
                }
                Instr::OP_VARARG => {
                    state.reg_vararg(ra, inst.vararg_count())?;
                }

                // Admission rejects unknown opcodes, including dead code.
                _ => unreachable!("verified bytecode contains an unknown opcode"),
            }
        }
    }
}

/// How [`Frame::run_frame`] left its activation.
enum Leave {
    /// A Lua callee was admitted; its frame goes on top.
    Call,
    /// The activation returned `count` results from register index `first`.
    Return { first: usize, count: usize },
}

impl State {
    /// Record `call_site`, the dispatch loop's cursor past the instruction
    /// that is about to call out, into the running activation's record.
    /// Tracebacks read it for every caller frame, and `print` reads the
    /// innermost one while a Rust function runs.
    ///
    /// Every path from a dispatch arm that can reach a call (into Lua or into
    /// a Rust function) must record first: `OP_CALL` and `OP_TFOR_CALL` in
    /// their arms, the table and length instructions first thing in their
    /// outlined stack paths (`get_field_slow`, `get_table_slow`,
    /// `set_field_slow`, `set_table_slow`, `len_slow`), so the cache hits
    /// and direct paths that call nothing do not pay for it. The innermost
    /// record is therefore stale while its activation runs, which nothing
    /// observes: a traceback skips the innermost record (the executing frame
    /// supplies that line), and `print` reads it only from a Rust function
    /// called through one of the recorded paths. Every suspended caller's
    /// record names the instruction that suspended it. The native call
    /// inventory under reference/ lists those paths; the traceback and
    /// print-location tests pin each opcode's caller line.
    #[inline]
    pub(super) fn record_call_site(&mut self, call_site: usize) {
        if let Some(call_info) = self.call_stack.last_mut() {
            call_info.ip = call_site;
        }
    }

    /// Prepare a call's operand window at `callee`: with fixed arguments the
    /// stack ends right after them, dropping the dead registers above, and
    /// with open arguments the producer already left the top there. Returns
    /// the argument count. The window must not cut through a register an
    /// open upvalue points at; compiled code always calls above every live
    /// local.
    #[inline(always)]
    pub(super) fn call_window(&mut self, callee: usize, args: ArgCount) -> Result<u8> {
        self.check_window(callee)?;
        match args {
            ArgCount::Fixed(n) => {
                self.stack.truncate(callee + 1 + usize::from(n));
                Ok(n)
            }
            ArgCount::Dynamic => {
                let actual = self.stack.len() - callee - 1;
                u8::try_from(actual).map_err(|_| {
                    self.error(super::ErrorKind::TooManyArguments {
                        limit: crate::limits::MAX_ARGUMENTS,
                    })
                })
            }
        }
    }

    /// Fail when an open upvalue points at or above `first`, the start of a
    /// window the next operation reshapes.
    #[inline(always)]
    pub(super) fn check_window(&self, first: usize) -> Result<()> {
        match self.open_upvalues.last() {
            Some(&(index, _)) if index >= first => Err(Error::internal_fault(
                super::super::error::InternalFault::WindowOverlapsOpenUpvalue,
            )),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_modulo_matches_lua54_floating_point_rules() {
        assert_eq!(lua_modulo(5.0, -3.0), -1.0);
        assert_eq!(lua_modulo(-5.0, 3.0), 1.0);
        assert_eq!(lua_modulo(1.0, f64::INFINITY), 1.0);
        assert_eq!(lua_modulo(-1.0, f64::INFINITY), f64::INFINITY);
        assert_eq!(lua_modulo(1.0, f64::NEG_INFINITY), f64::NEG_INFINITY);
        assert!(lua_modulo(f64::NAN, 1.0).is_nan());
        assert!(lua_modulo(1.0, f64::NAN).is_nan());
        assert!(lua_modulo(-0.0, 3.0).is_sign_negative());
    }

    #[test]
    fn jump_accepts_the_most_negative_offset() {
        let span = 1usize << 23;
        assert_eq!(Frame::jumped(span, -(1 << 23)), 0);
    }

    #[test]
    fn verifier_rejects_end_of_bytecode_jump() {
        let bytecode = Bytecode {
            code: vec![Instr::jump(1), Instr::ret(0, RetCount::Fixed(0))],
            line_info: vec![1, 1],
            ..Bytecode::default()
        };
        let error = crate::compiler::verify::validate_bytecode(&bytecode)
            .expect_err("jump to end must be rejected before dispatch");
        assert_eq!(error.instruction, Some(0));
        assert_eq!(
            error.reason,
            crate::compiler::verify::ValidationReason::JumpTargetOutOfRange
        );
    }

    #[test]
    fn documented_opcode_charges_match_static_units() {
        let docs = include_str!("../../docs/budget.md");
        let section = docs
            .split_once("## Static opcode charges")
            .map(|(_, rest)| rest)
            .expect("budget documentation contains the opcode charge table");
        let mut classes = Vec::new();

        for opcode in u8::MIN..=u8::MAX {
            if !Instr::known_opcode(opcode) {
                continue;
            }

            let class_name = format!("{:?}", Instr::cost_class(opcode));
            let row_prefix = format!("| `{class_name}` |");
            let row = section
                .lines()
                .find(|line| line.starts_with(&row_prefix))
                .expect("each opcode cost class has a documented row");
            if !classes.contains(&class_name) {
                classes.push(class_name.clone());
            }

            let documented_units = row
                .split('|')
                .nth(2)
                .expect("opcode charge row has a units column")
                .trim();
            if class_name == "ArrayElements" {
                assert_eq!(documented_units, "encoded count");
                assert_eq!(Instr::set_list(0, Some(7), 0).static_units(), 7);
            } else {
                let documented_units = documented_units
                    .parse::<u64>()
                    .expect("fixed opcode charge is a unit count");
                assert_eq!(Instr::fixed_units(opcode), documented_units, "{class_name}");
            }
        }

        let documented_rows = section
            .lines()
            .filter(|line| line.starts_with("| `"))
            .count();
        assert_eq!(documented_rows, classes.len() + 1);
        assert_eq!(Instr::set_list(0, None, 0).static_units(), 0);
    }

    #[test]
    fn reentry_accounting_sources_cover_the_intended_dispatch_opcodes() {
        let cases = [
            ("callable()", Instr::OP_CALL, "CALL"),
            (
                "for item in iterator do end",
                Instr::OP_TFOR_CALL,
                "TFOR_CALL",
            ),
            ("local result = #lengthy", Instr::OP_LEN, "LEN"),
            (
                "local result = indexed.field",
                Instr::OP_GET_FIELD,
                "GET_FIELD",
            ),
            (
                "local result = indexed[left]",
                Instr::OP_GET_TABLE,
                "GET_TABLE",
            ),
            (
                "new_indexed.field = right",
                Instr::OP_SET_FIELD,
                "SET_FIELD",
            ),
            (
                "other.field, new_indexed.field = left, right",
                Instr::OP_SET_FIELD,
                "SET_FIELD in a multiple assignment",
            ),
            (
                "new_indexed[left] = right",
                Instr::OP_SET_TABLE,
                "SET_TABLE",
            ),
            ("receiver:method()", Instr::OP_SELF, "SELF"),
        ];

        for (source, expected_opcode, name) in cases {
            let bytecode = crate::compiler::compile(source, None)
                .unwrap_or_else(|error| panic!("{name} source compiles: {error}"));
            assert!(
                bytecode
                    .code
                    .iter()
                    .any(|instruction| instruction.opcode() == expected_opcode),
                "{name} source must exercise its dispatch opcode"
            );
        }
    }
}
