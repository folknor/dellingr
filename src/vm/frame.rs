use std::cell::Cell;
use std::sync::Arc;

// The constant folder and runtime dispatch use the arithmetic functions
// declared with each opcode in `instr.rs`.
use crate::numeral::{lua_add, lua_div, lua_modulo, lua_mul, lua_sub};

use super::super::error::Error;
use super::Bytecode;
use super::BytecodeRuntime;
use super::Instr;
use super::Result;
use super::State;
use super::Val;
use super::object::{Captures, UpvalueRef};
use crate::cost_model::Charge;
use crate::instr::{ArgCount, CompareOp, RetCount};

/// A `Frame` represents a single stack-frame of a Lua function.
///
/// The frame's registers are the stack slots `[stack_bottom, top)`, reserved
/// and nil-filled at admission, with `FRAME_HEADROOM` checked free above
/// them. While its bytecode runs, the stack normally has exactly `top` slots.
/// Its length can differ while an instruction's call window is open, and
/// between a verified open-result producer and its next-instruction consumer,
/// when it can be shorter or longer than `top`.
///
/// The frame owns its code handles rather than naming a State-owned record
/// by id: the dispatch loop holds them by shared reference while it mutates
/// the State, which an id resolved through the State cannot give without a
/// lookup per access or a raw pointer whose validity would rest on rooting
/// (and a Rust tail callee may collect the retired caller's closure while its
/// activation record remains).
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
    /// The upvalues captured by this closure: a clone of the `Closure`'s
    /// list (a copy for up to one capture, else a shared `Arc`), never
    /// mutated through the frame; writes go through the `UpvaluePool` slots.
    pub(super) upvalues: Captures,
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
        upvalues: Captures,
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

    /// The published cursor, the index of the instruction after the one the
    /// frame last executed. Error location reads it once the dispatch loop
    /// has published it on the error's way out (see `ip`).
    pub(super) fn cursor(&self) -> usize {
        self.ip.get()
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

    /// Take a jump by a verified `offset`. `JMP`, compare-and-branch and
    /// test opcodes, `FOR_LOOP` and `TFOR_LOOP` poll for interrupt and GC
    /// pressure on backward jumps. Admission requires `FOR_PREP` to go
    /// forward, so every backward transfer passes through this checkpoint.
    /// The poll comes before the cursor moves, so an interruption names the
    /// jumping instruction (`locate_fault` reads `cursor - 1`).
    #[inline(always)]
    pub(super) fn branch(state: &mut State, ip: &mut usize, offset: i32) -> Result<()> {
        if offset < 0 {
            state.poll_gc_checkpoint()?;
        }
        *ip = Self::jumped(*ip, offset);
        Ok(())
    }

    /// The generic for's back edge `step`, with `ip` already past it: the
    /// `TFOR_LOOP` arm and the `TFOR_CALL` arm that runs it as the real next
    /// instruction share this body. `TFOR_LOOP` is free, so neither entry
    /// charges. Poll before the control moves, as FOR_LOOP does. A function
    /// rather than a macro: a dev build gives each expansion's locals their
    /// own slots in the dispatch frame, live across every native re-entry.
    #[inline]
    fn tfor_loop(state: &mut State, ip: &mut usize, base: usize, step: Instr) -> Result<()> {
        const _: () = assert!(Instr::fixed_charge(Instr::OP_TFOR_LOOP).is_none());
        let rl = base + step.a() as usize;
        if !matches!(state.stack[rl + 3], Val::Nil) {
            Self::branch(state, ip, step.sj())?;
            state.stack[rl + 2] = state.stack[rl + 3];
        }
        Ok(())
    }

    /// `tfor_loop` run from the `TFOR_CALL` arm. When the step continues
    /// the loop from key 1, the first step of `ipairs(t)`, the rest may run
    /// as a typed `ipairs` region (`State::enter_ipairs_region`); a key of
    /// nil means the edge was not taken. Inline only without debug
    /// assertions, as `State::tfor_fast_step` is: in a dev build its locals
    /// stay out of the dispatch frame, which is live across every native
    /// re-entry.
    #[cfg_attr(not(debug_assertions), inline(always))]
    fn tfor_fused_loop(
        state: &mut State,
        frame: &Frame,
        ip: &mut usize,
        base: usize,
        step: Instr,
        local_cost: &mut u64,
    ) -> Result<()> {
        Self::tfor_loop(state, ip, base, step)?;
        let rl = base + step.a() as usize;
        if let Val::Num(key) = state.stack[rl + 3]
            && key.to_bits() == 1.0f64.to_bits()
        {
            state.enter_ipairs_region(frame, ip, local_cost)?;
        }
        Ok(())
    }

    #[must_use]
    pub(super) fn nested_bytecode(&self, i: u16) -> &Arc<Bytecode> {
        &self.bytecode.nested[i as usize]
    }

    #[must_use]
    pub(super) fn get_number_constant(&self, i: u16) -> f64 {
        self.bytecode.number_literals[i as usize]
    }

    /// Publish the invocation's pending batch before code outside the
    /// dispatch loop can touch the meter (a Rust callee, a leaf, an iterator,
    /// a metamethod handler). Table helpers call this only after a direct
    /// access path has been ruled out. The resolvers take the batch for that
    /// reason: hoisting their flush to the caller would still need it in the
    /// `reg_*` helpers and save no meter write.
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

    /// Charge one opcode's fixed charge at the State's weight, skipping a
    /// zero weight. The same batching rule as `run_frame`'s `add_cost!`.
    /// A function rather than macro text in every dispatch arm: a dev build
    /// gives each expansion's weight, sum and threshold temporaries their own
    /// slots in the dispatch frame, live across every native re-entry, and
    /// the per-arm copies were most of the dev `Frame::run` frame growth when
    /// weights became configurable (2080 bytes a dev level when this moved
    /// here). Forced inline only without debug assertions, so release keeps
    /// the charge inlined at each arm; its native frames did not move.
    #[cfg_attr(not(debug_assertions), inline(always))]
    fn charge_fixed(
        state: &mut State,
        local_cost: &mut u64,
        #[cfg(feature = "cost-check-interval")] ip: usize,
        charge: Charge,
    ) -> Result<()> {
        let units = state.cost.weight(charge);
        if units != 0 {
            // A weight is at most `u32::MAX` and pending cost stays below the
            // flush threshold, at most `COST_CHECK_INTERVAL`, so the
            // production sum cannot overflow. The debug interval override
            // accepts any positive interval, so that build saturates.
            #[cfg(not(feature = "cost-check-interval"))]
            let next = *local_cost + units;
            #[cfg(feature = "cost-check-interval")]
            let next = (*local_cost).saturating_add(units);
            if next >= state.cost_flush_at() {
                Self::settle_cost(state, local_cost, next)?;
            } else {
                *local_cost = next;
            }
            #[cfg(feature = "cost-check-interval")]
            crate::cost_meter::interval_test::record(ip, units);
        }
        Ok(())
    }

    /// Continue a template constructor from the instruction after its
    /// `NEW_TABLE_TEMPLATE`, which has just allocated the table into
    /// register `table`: execute the leading run of static value loads and
    /// pinned writes into that table in this one call instead of one dispatch
    /// each. `ip` is the dispatch cursor; on return it names the first
    /// instruction the run did not execute, and on a refused charge the
    /// refused write's own cursor, so the fault is located at that field.
    ///
    /// The run is exactly the canonical execution, with the dispatch and the
    /// receiver's type check removed. Every scratch-register write still
    /// happens (a freed scratch register stays a GC root, docs/limits.md), and
    /// each pinned write admits its `TableWrite` charge separately, at its
    /// own cursor, with the zero-weight bypass of `fixed_cost!`, so the
    /// charged prefix, the pending batch and `cost_used` are those of
    /// dispatching it. The loads are free and infallible and nothing in the
    /// run allocates, calls or polls an interrupt. The receiver check can go
    /// because this call starts at the allocation: the table is the one just
    /// written to `table`, and the run ends at any instruction that could
    /// overwrite that register (a load into it, a nil fill covering it) or
    /// write another table. Jumps into the run's middle reach the canonical
    /// handlers, which check as before; the key and entry checks of
    /// `Table::init_at_index` and its fallback insert stay.
    ///
    /// Only the leading run is taken. A pinned write after a charged or
    /// fallible value (`ADD_RK` in `examples/alloc/record_tables.lua`) is
    /// left to dispatch: fusing across it needs a witness that the register
    /// still holds this table.
    ///
    /// Declined in its favour: a `DUPTABLE`-style opcode that evaluates every
    /// value first and fills the table at the end, and a bytecode plan that
    /// prefills literals into the prototype. The first moves `NewTable` and
    /// the write charges past value work that can charge, fail, allocate or
    /// call, so `cost_used` at an error, collection timing and object ids
    /// move. The second skips scratch-register writes that stay GC roots
    /// after the constructor, and a single opcode cannot fail at the refused
    /// field's line. Measured on bygg in matched interleaved pairs against
    /// 8b07d97, 3 rounds best of 3: `static_records` 1601 to 1471 ms (3 of 3),
    /// `record_tables` 1577 to 1553 ms, `short_tables` and `method_step` flat.
    /// Release native bytes per level unchanged; dev 16 bytes more.
    #[inline(never)]
    pub(super) fn template_init_run(
        frame: &Frame,
        state: &mut State,
        local_cost: &mut u64,
        ip: &mut usize,
        table: usize,
        upvalues: &[UpvalueRef],
    ) -> Result<()> {
        let code: &[Instr] = &frame.bytecode.code;
        let base = frame.stack_bottom;
        let Some(ptr) = state.stack[table].as_object_ptr() else {
            return Ok(());
        };
        // bounded: one step per instruction, ending at the code's end.
        while let Some(&inst) = code.get(*ip) {
            let ra = base + inst.a() as usize;
            match inst.opcode() {
                Instr::OP_LOAD_NIL => {
                    let end = ra + inst.b() as usize;
                    if (ra..end).contains(&table) {
                        return Ok(());
                    }
                    state.stack[ra..end].fill(Val::Nil);
                }
                Instr::OP_LOAD_BOOL if ra != table => {
                    state.stack[ra] = Val::Bool(inst.b() != 0);
                }
                Instr::OP_LOAD_NUM if ra != table => {
                    state.stack[ra] = Val::Num(frame.get_number_constant(inst.b()));
                }
                Instr::OP_LOAD_STR if ra != table => {
                    state.stack[ra] = frame.literal(inst.b());
                }
                Instr::OP_GET_UPVAL if ra != table => {
                    state.stack[ra] = state.upvalue_value(upvalues, inst.b());
                }
                Instr::OP_INIT_FIELD_PINNED if ra == table => {
                    *ip += 1;
                    const CHARGE: Option<Charge> = Instr::fixed_charge(Instr::OP_INIT_FIELD_PINNED);
                    if let Some(charge) = CHARGE {
                        let units = state.cost.weight(charge);
                        if units != 0 {
                            let next = (*local_cost).saturating_add(units);
                            if next >= state.cost_flush_at() {
                                Self::settle_cost(state, local_cost, next)?;
                            } else {
                                *local_cost = next;
                            }
                            #[cfg(feature = "cost-check-interval")]
                            crate::cost_meter::interval_test::record(*ip, units);
                        }
                    }
                    let key = frame.literal(inst.b());
                    let val = state.stack[base + inst.c() as usize];
                    let mut tbl = state
                        .heap
                        .edit_table(ptr)
                        .expect("the run's receiver is the table its allocation just made");
                    if !tbl.init_at_index(usize::from(inst.d()), key, val) {
                        tbl.insert(key, val)?;
                    }
                    continue;
                }
                _ => return Ok(()),
            }
            *ip += 1;
        }
        Ok(())
    }

    /// The `RetCount` operand of the `OP_CALL` this suspended frame is
    /// waiting on. A suspended frame's `ip` points just past that call, and
    /// its bytecode is immutable, so the instruction can be read back rather
    /// than stored per activation. A frame never waits on an `OP_TAILCALL`:
    /// a Lua callee replaces it, and a Rust callee completes in the arm.
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
    /// Fixed opcode charges are one unit of `Instr::fixed_charge` at the
    /// State's weight. SET_LIST charges its live element count of
    /// `Instr::SET_LIST_ELEMENT_CHARGE` at once (the analyzer only knows its
    /// encoded count).
    /// Calls and access helpers can additionally charge native work, including
    /// strings, patterns, table traversal and metamethod callbacks.
    pub(super) fn run(frames: &mut Vec<Frame>, state: &mut State) -> Result<(usize, usize)> {
        // Batch cost checking: the invocation owns one pending batch across
        // all its Lua frames. Lua calls, tail calls and returns into a Lua
        // caller carry it forward, since admission and return never touch
        // the meter. It is published before anything (a Rust callee, a
        // library leaf, a callable object, an iterator, a metamethod
        // handler) can read the published balance or charge separately, and
        // settled here on every `Result` exit. In-loop helpers that invoke
        // nothing (cache hits, the `next`/`ipairs` generic-for steps) leave
        // it pending.
        let mut pending: u64 = 0;
        let result = crate::native_call!(
            "run->eval_batched",
            Self::eval_batched(frames, state, &mut pending)
        );
        if pending > 0 {
            // The work that already executed is charged on every exit, Ok
            // or Err, so `cost_used` after a return or a failed callback
            // does not depend on the batching interval. A positive batch
            // stays below the flush threshold, which a finite budget caps at
            // its room (add_cost! settles first when it would reach it), so
            // this charge is always admitted; a refused settle leaves
            // nothing pending. On error the original error propagates.
            let admitted = state.cost_meter().consume(pending);
            debug_assert!(admitted, "a pending batch always fits the budget");
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
            let leave = crate::native_call!(
                "eval_batched->run_frame",
                Self::run_frame(frame, state, local_cost, &mut ip)
            );
            frame.ip.set(ip);
            match leave? {
                Leave::Call => frames.extend(std::iter::once_with(|| state.admitted_frame())),
                Leave::TailCall => state.replace_tail_frame(frames),
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
    ///
    /// One `match` is the dispatch on purpose. Threaded dispatch, where each
    /// handler tail-calls the next, was considered and refused: Rust has no
    /// guaranteed tail transfer on the supported toolchain (`become` is
    /// unstable even on current nightly), and an ordinary call between
    /// handlers is not guaranteed to be eliminated at any optimization level
    /// (dev builds never do it), so one surviving non-tail edge on a handler
    /// cycle grows the native stack per executed instruction and breaks the
    /// flat-native-stack invariant. A table of handler pointers called from a
    /// loop keeps one shared indirect site, so it buys no per-predecessor
    /// prediction and adds a call and return per opcode. Reopen this only
    /// with a guaranteed tail transfer on every supported toolchain (or
    /// per-ISA assembly) and a current measurement attributing cost to this
    /// branch. Predecessor-specific dispatch on stable Rust belongs here
    /// instead: after a handler, test the actual next opcode and enter its
    /// body directly, which is execution-side fusion keyed on the real next
    /// instruction.
    #[inline(always)]
    fn run_frame(
        frame: &Frame,
        state: &mut State,
        local_cost: &mut u64,
        ip: &mut usize,
    ) -> Result<Leave> {
        let code: &[Instr] = &frame.bytecode.code;
        let base = frame.stack_bottom;
        // The capture list is immutable once built and the frame outlives
        // the activation, so its representation is selected once here
        // rather than at every GET_UPVAL and SET_UPVAL. The slice stays live
        // across the dispatch loop: measured when it landed, 16 bytes more
        // per native level in release (32 fewer in dev), against about 7% on
        // a loop dispatching closed captures (bench/closed_mode), which was
        // judged worth it inside the native stack envelope (docs/limits.md).
        let upvalues = frame.upvalues.as_slice();
        /// Macro to accumulate cost and flush when threshold is reached.
        /// `@unbounded $cost` is a charge of any size (a SET_LIST element
        /// count times its weight); fixed charges go through `charge_fixed`,
        /// which applies the same rule.
        macro_rules! add_cost {
            (@next $next:expr, $cost:expr) => {{
                let next = $next;
                // This decides when to flush, not whether to accept a charge.
                // consume_cost owns the runtime meter's admission rule. The
                // threshold is the batching interval, lowered for a finite
                // budget to what it has left, so pending cost is settled
                // before it would exhaust the budget: the interval does not
                // change the charged prefix or `cost_used` at
                // `BudgetExceeded`, and a spent budget (threshold 0) sends the
                // next positive charge straight to admission. Any other error
                // settles the pending batch in `Frame::run`.
                if next >= state.cost_flush_at() {
                    Self::settle_cost(state, local_cost, next)?;
                } else {
                    *local_cost = next;
                }
                #[cfg(feature = "cost-check-interval")]
                crate::cost_meter::interval_test::record(*ip, $cost);
            }};
            (@unbounded $cost:expr) => {
                add_cost!(@next (*local_cost).saturating_add($cost), $cost)
            };
        }

        // Which charge an arm makes is a compile-time constant; its weight is
        // the State's, fixed when the State was built. A free opcode, or a
        // charge weighted 0 (every charge, when costing is off), must not run
        // add_cost!(0): its budget check could reject structural work. Range
        // arms retain their shared body and require all members to have the
        // same fixed charge.
        macro_rules! fixed_cost {
            ($first:expr $(, $last:expr)?) => {{
                const CHARGE: Option<Charge> = Instr::fixed_charge($first);
                $(
                    const _: () = {
                        const fn key(charge: Option<Charge>) -> i32 {
                            match charge {
                                Some(charge) => charge as i32,
                                None => -1,
                            }
                        }
                        let mut opcode = $first as usize;
                        while opcode <= $last as usize {
                            assert!(key(Instr::fixed_charge(opcode as u8)) == key(CHARGE));
                            opcode += 1;
                        }
                    };
                )?
                if let Some(charge) = CHARGE {
                    #[cfg(not(feature = "cost-check-interval"))]
                    Self::charge_fixed(state, local_cost, charge)?;
                    #[cfg(feature = "cost-check-interval")]
                    Self::charge_fixed(state, local_cost, *ip, charge)?;
                }
            }};
        }

        // One arm per arithmetic opcode, each a direct call to its function
        // item with its operand kinds fixed.
        macro_rules! arith {
            ($inst:ident, $f:path, rr) => {{
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
                let lhs = state.stack[base + $inst.b() as usize];
                let y = frame.get_number_constant($inst.c());
                match lhs {
                    Val::Num(x) => state.stack[base + $inst.a() as usize] = Val::Num($f(x, y)),
                    _ => return Err(state.arithmetic_operands_error(lhs, Val::Num(y))),
                }
            }};
            ($inst:ident, $f:path, kr) => {{
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
                    fixed_cost!(Instr::OP_MOVE);
                    state.stack[ra] = state.stack[base + inst.b() as usize];
                }
                Instr::OP_LOAD_NIL => {
                    fixed_cost!(Instr::OP_LOAD_NIL);
                    state.stack[ra..ra + inst.b() as usize].fill(Val::Nil);
                }
                Instr::OP_LOAD_BOOL => {
                    fixed_cost!(Instr::OP_LOAD_BOOL);
                    state.stack[ra] = Val::Bool(inst.b() != 0);
                }
                Instr::OP_LOAD_NUM => {
                    fixed_cost!(Instr::OP_LOAD_NUM);
                    state.stack[ra] = Val::Num(frame.get_number_constant(inst.b()));
                }
                Instr::OP_LOAD_STR => {
                    fixed_cost!(Instr::OP_LOAD_STR);
                    state.stack[ra] = frame.literal(inst.b());
                }

                // Upvalues
                Instr::OP_GET_UPVAL => {
                    fixed_cost!(Instr::OP_GET_UPVAL);
                    state.stack[ra] = state.upvalue_value(upvalues, inst.b());
                }
                Instr::OP_SET_UPVAL => {
                    fixed_cost!(Instr::OP_SET_UPVAL);
                    let val = state.stack[ra];
                    state.set_upvalue_value(upvalues, inst.b(), val);
                }
                Instr::OP_CLOSE => {
                    fixed_cost!(Instr::OP_CLOSE);
                    state.close_upvalues(ra);
                }

                // Globals and builtins
                Instr::OP_GET_GLOBAL => {
                    fixed_cost!(Instr::OP_GET_GLOBAL);
                    state.stack[ra] = state.read_global(frame, inst.b(), inst.cache_slot());
                }
                Instr::OP_SET_GLOBAL => {
                    fixed_cost!(Instr::OP_SET_GLOBAL);
                    state.write_global(frame, inst.b(), inst.cache_slot(), ra);
                }
                Instr::OP_GET_BUILTIN => {
                    fixed_cost!(Instr::OP_GET_BUILTIN);
                    state.stack[ra] = state.global_env.read_builtin(&state.heap, inst.b() as u8);
                }
                Instr::OP_SET_BUILTIN => {
                    fixed_cost!(Instr::OP_SET_BUILTIN);
                    state.global_env.write_builtin(
                        &mut state.heap,
                        inst.b() as u8,
                        &state.stack[ra],
                    );
                }

                // Table creation
                Instr::OP_NEW_TABLE => {
                    fixed_cost!(Instr::OP_NEW_TABLE);
                    state.stack[ra] = state.alloc_table();
                }
                Instr::OP_NEW_TABLE_PRESIZED => {
                    fixed_cost!(Instr::OP_NEW_TABLE_PRESIZED);
                    // B is a hash-capacity hint: the constructor's keyed-field
                    // count. The array part is reserved per SET_LIST batch.
                    state.stack[ra] = state.alloc_table_with_capacity(inst.b() as usize);
                }
                Instr::OP_NEW_TABLE_TEMPLATE => {
                    fixed_cost!(Instr::OP_NEW_TABLE_TEMPLATE);
                    state.reg_new_table_template(frame, ra, inst.b());
                    Self::template_init_run(frame, state, local_cost, ip, ra, upvalues)?;
                }

                // Table reads
                // Table reads. These and the table writes and length below
                // pass the cursor down: their handlers record it as the call
                // site only on the path that can call out
                // (`State::record_call_site`).
                Instr::OP_GET_FIELD => {
                    fixed_cost!(Instr::OP_GET_FIELD);
                    let receiver = state.stack[base + inst.b() as usize];
                    crate::native_call!(
                        "run_frame->reg_get_field:field",
                        state.reg_get_field(frame, inst, ra, receiver, local_cost, *ip)
                    )?;
                }
                Instr::OP_SELF => {
                    fixed_cost!(Instr::OP_SELF);
                    let receiver = state.stack[base + inst.b() as usize];
                    state.stack[ra + 1] = receiver;
                    crate::native_call!(
                        "run_frame->reg_get_field:self",
                        state.reg_get_field(frame, inst, ra, receiver, local_cost, *ip)
                    )?;
                }
                Instr::OP_GET_TABLE => {
                    fixed_cost!(Instr::OP_GET_TABLE);
                    crate::native_call!(
                        "run_frame->reg_get_table",
                        state.reg_get_table(frame, inst, ra, base, local_cost, *ip)
                    )?;
                }

                // Table writes
                Instr::OP_SET_FIELD => {
                    fixed_cost!(Instr::OP_SET_FIELD);
                    crate::native_call!(
                        "run_frame->reg_set_field",
                        state.reg_set_field(
                            frame,
                            inst,
                            ra,
                            base + inst.c() as usize,
                            local_cost,
                            *ip,
                        )
                    )?;
                }
                Instr::OP_SET_TABLE => {
                    fixed_cost!(Instr::OP_SET_TABLE);
                    crate::native_call!(
                        "run_frame->reg_set_table",
                        state.reg_set_table(
                            ra,
                            base + inst.b() as usize,
                            base + inst.c() as usize,
                            local_cost,
                            *ip,
                        )
                    )?;
                }
                Instr::OP_INIT_FIELD => {
                    fixed_cost!(Instr::OP_INIT_FIELD);
                    state.reg_init_field(frame, ra, inst.b(), base + inst.c() as usize)?;
                }
                Instr::OP_INIT_FIELD_PINNED => {
                    fixed_cost!(Instr::OP_INIT_FIELD_PINNED);
                    state.reg_init_field_pinned(
                        frame,
                        ra,
                        inst.b(),
                        base + inst.c() as usize,
                        inst.d(),
                    )?;
                }
                Instr::OP_INIT_INDEX => {
                    fixed_cost!(Instr::OP_INIT_INDEX);
                    state.reg_init_index(ra, base + inst.b() as usize, base + inst.c() as usize)?;
                }
                Instr::OP_SET_LIST => {
                    fixed_cost!(Instr::OP_SET_LIST);
                    let count = match inst.set_list_count() {
                        Some(count) => usize::from(count),
                        None => state.stack.len() - (ra + 1),
                    };
                    let units = state
                        .cost
                        .cost(Instr::SET_LIST_ELEMENT_CHARGE, count as u64);
                    if units != 0 {
                        add_cost!(@unbounded units);
                    }
                    state.reg_set_list(ra, count, inst.c())?;
                    if inst.set_list_count().is_none() {
                        state.stack.resize(frame.top, Val::Nil);
                    }
                }

                // Arithmetic
                Instr::OP_ADD_RR => {
                    fixed_cost!(Instr::OP_ADD_RR);
                    arith!(inst, lua_add, rr);
                }
                Instr::OP_ADD_RK => {
                    fixed_cost!(Instr::OP_ADD_RK);
                    arith!(inst, lua_add, rk);
                }
                Instr::OP_ADD_KR => {
                    fixed_cost!(Instr::OP_ADD_KR);
                    arith!(inst, lua_add, kr);
                }
                Instr::OP_SUB_RR => {
                    fixed_cost!(Instr::OP_SUB_RR);
                    arith!(inst, lua_sub, rr);
                }
                Instr::OP_SUB_RK => {
                    fixed_cost!(Instr::OP_SUB_RK);
                    arith!(inst, lua_sub, rk);
                }
                Instr::OP_SUB_KR => {
                    fixed_cost!(Instr::OP_SUB_KR);
                    arith!(inst, lua_sub, kr);
                }
                Instr::OP_MUL_RR => {
                    fixed_cost!(Instr::OP_MUL_RR);
                    arith!(inst, lua_mul, rr);
                }
                Instr::OP_MUL_RK => {
                    fixed_cost!(Instr::OP_MUL_RK);
                    arith!(inst, lua_mul, rk);
                }
                Instr::OP_MUL_KR => {
                    fixed_cost!(Instr::OP_MUL_KR);
                    arith!(inst, lua_mul, kr);
                }
                Instr::OP_DIV_RR => {
                    fixed_cost!(Instr::OP_DIV_RR);
                    arith!(inst, lua_div, rr);
                }
                Instr::OP_DIV_RK => {
                    fixed_cost!(Instr::OP_DIV_RK);
                    arith!(inst, lua_div, rk);
                }
                Instr::OP_DIV_KR => {
                    fixed_cost!(Instr::OP_DIV_KR);
                    arith!(inst, lua_div, kr);
                }
                Instr::OP_MOD_RR => {
                    fixed_cost!(Instr::OP_MOD_RR);
                    arith!(inst, lua_modulo, rr);
                }
                Instr::OP_MOD_RK => {
                    fixed_cost!(Instr::OP_MOD_RK);
                    arith!(inst, lua_modulo, rk);
                }
                Instr::OP_MOD_KR => {
                    fixed_cost!(Instr::OP_MOD_KR);
                    arith!(inst, lua_modulo, kr);
                }
                Instr::OP_POW_RR => {
                    fixed_cost!(Instr::OP_POW_RR);
                    arith!(inst, crate::crmath::pow, rr);
                }
                Instr::OP_POW_RK => {
                    fixed_cost!(Instr::OP_POW_RK);
                    arith!(inst, crate::crmath::pow, rk);
                }
                Instr::OP_POW_KR => {
                    fixed_cost!(Instr::OP_POW_KR);
                    arith!(inst, crate::crmath::pow, kr);
                }
                Instr::OP_UNM => {
                    fixed_cost!(Instr::OP_UNM);
                    match state.stack[base + inst.b() as usize] {
                        Val::Num(n) => state.stack[ra] = Val::Num(-n),
                        other => return Err(state.arithmetic_operands_error(other, other)),
                    }
                }
                Instr::OP_NOT => {
                    fixed_cost!(Instr::OP_NOT);
                    state.stack[ra] = Val::Bool(!state.stack[base + inst.b() as usize].truthy());
                }
                Instr::OP_LEN => {
                    fixed_cost!(Instr::OP_LEN);
                    crate::native_call!(
                        "run_frame->reg_len",
                        state.reg_len(ra, base + inst.b() as usize, local_cost, *ip)
                    )?;
                }

                // The bytes `..` emits are deliberately uncharged: the
                // budget is a game-design instrument rather than a CPU
                // meter (docs/budget.md), and `..` is structural syntax
                // that a legible refactor must not make more expensive.
                // The size is still bounded by `checked_string_growth`
                // (MAX_STRING_BYTES), so the uncharged work is capped.
                // String library calls leave the bytes they read and produce
                // uncharged too (docs/budget.md, "String costs").
                Instr::OP_CONCAT => {
                    fixed_cost!(Instr::OP_CONCAT);
                    state.reg_concat(ra, base + inst.b() as usize, inst.c() as usize)?;
                }

                // Comparisons
                Instr::OP_LT..=Instr::OP_NE => {
                    fixed_cost!(Instr::OP_LT, Instr::OP_NE);
                    let op = CompareOp::from_value_opcode(inst.opcode())
                        .expect("the range holds the value comparisons");
                    let lhs = state.stack[base + inst.b() as usize];
                    let rhs = state.stack[base + inst.c() as usize];
                    state.stack[ra] = Val::Bool(state.compare_vals(op, lhs, rhs)?);
                }

                // Control flow
                Instr::OP_JMP => {
                    fixed_cost!(Instr::OP_JMP);
                    Self::branch(state, ip, inst.sj())?;
                }
                // Compare-and-branch: jump when the comparison is false.
                // One range arm with the variant decode outlined - per-
                // variant arms measurably perturbed the dispatch loop's
                // codegen in the stack VM. Matching a number pair on `op`
                // inline, as the literal arm below does, instead of
                // `compare_vals`'s `ordering` and `partial_cmp`, was measured
                // together with a branch-if-true family on the `cull`
                // workload (see `cond_jump` in codegen.rs) and was flat.
                Instr::OP_BR_LT..=Instr::OP_BR_NE => {
                    fixed_cost!(Instr::OP_BR_LT, Instr::OP_BR_NE);
                    let op = CompareOp::from_branch_opcode(inst.opcode())
                        .expect("the range holds the branch comparisons");
                    let lhs = state.stack[ra];
                    let rhs = state.stack[base + inst.b() as usize];
                    if !state.compare_vals(op, lhs, rhs)? {
                        Self::branch(state, ip, inst.sj())?;
                    }
                }
                // Against a number literal: a number operand compares
                // here with IEEE semantics (every ordering false on NaN,
                // which is what `compare_vals` answers); anything else takes
                // the shared helper for its equality or type error.
                Instr::OP_BR_LT_RK..=Instr::OP_BR_NE_RK => {
                    fixed_cost!(Instr::OP_BR_LT_RK, Instr::OP_BR_NE_RK);
                    let op = CompareOp::from_branch_number_opcode(inst.opcode())
                        .expect("the range holds the number-literal branches");
                    let lhs = state.stack[ra];
                    let k = frame.get_number_constant(inst.b());
                    #[expect(
                        clippy::float_cmp,
                        reason = "Lua `==` on numbers is exact IEEE equality"
                    )]
                    let holds = match lhs {
                        Val::Num(x) => match op {
                            CompareOp::Lt => x < k,
                            CompareOp::Le => x <= k,
                            CompareOp::Gt => x > k,
                            CompareOp::Ge => x >= k,
                            CompareOp::Eq => x == k,
                            CompareOp::Ne => x != k,
                        },
                        _ => state.compare_vals(op, lhs, Val::Num(k))?,
                    };
                    if !holds {
                        Self::branch(state, ip, inst.sj())?;
                    }
                }
                // Raw equality with an interned literal is handle equality.
                Instr::OP_BR_EQ_RS => {
                    fixed_cost!(Instr::OP_BR_EQ_RS);
                    if state.stack[ra] != frame.literal(inst.b()) {
                        Self::branch(state, ip, inst.sj())?;
                    }
                }
                Instr::OP_BR_NE_RS => {
                    fixed_cost!(Instr::OP_BR_NE_RS);
                    if state.stack[ra] == frame.literal(inst.b()) {
                        Self::branch(state, ip, inst.sj())?;
                    }
                }
                Instr::OP_BR_EQ_NIL => {
                    fixed_cost!(Instr::OP_BR_EQ_NIL);
                    if !matches!(state.stack[ra], Val::Nil) {
                        Self::branch(state, ip, inst.sj())?;
                    }
                }
                Instr::OP_BR_NE_NIL => {
                    fixed_cost!(Instr::OP_BR_NE_NIL);
                    if matches!(state.stack[ra], Val::Nil) {
                        Self::branch(state, ip, inst.sj())?;
                    }
                }
                Instr::OP_TEST_FALSE => {
                    fixed_cost!(Instr::OP_TEST_FALSE);
                    if !state.stack[ra].truthy() {
                        Self::branch(state, ip, inst.sj())?;
                    }
                }
                Instr::OP_TEST_TRUE => {
                    fixed_cost!(Instr::OP_TEST_TRUE);
                    if state.stack[ra].truthy() {
                        Self::branch(state, ip, inst.sj())?;
                    }
                }

                // `for` loops
                Instr::OP_FOR_PREP => {
                    fixed_cost!(Instr::OP_FOR_PREP);
                    state.instr_for_prep(frame, ra, ip, inst.sj(), local_cost)?;
                }
                Instr::OP_FOR_LOOP => {
                    fixed_cost!(Instr::OP_FOR_LOOP);
                    state.instr_for_loop(ra, ip, inst.sj())?;
                }
                Instr::OP_TFOR_CALL => {
                    fixed_cost!(Instr::OP_TFOR_CALL);
                    // A next/ipairs step stays in the loop with the batch
                    // pending. Every other step calls out, so it records the
                    // call site and publishes the batch first.
                    if !state.tfor_fast_step(
                        ra,
                        inst.b() as u8,
                        inst.cache_slot(),
                        &frame.runtime.caches,
                    ) {
                        state.record_call_site(*ip);
                        Self::flush_local_cost(state, local_cost)?;
                        crate::native_call!(
                            "run_frame->instr_tfor_call",
                            state.instr_tfor_call(ra, inst.b() as u8)
                        )?;
                    }
                    // Fusion keyed on the real next instruction: compiled
                    // code always follows TFOR_CALL with its TFOR_LOOP, so
                    // run that free back edge here rather than dispatch it.
                    // The cursor moves past it first, so its poll names it.
                    if let Some(&step) = code.get(*ip)
                        && step.opcode() == Instr::OP_TFOR_LOOP
                    {
                        *ip += 1;
                        Self::tfor_fused_loop(state, frame, ip, base, step, local_cost)?;
                    }
                }
                Instr::OP_TFOR_LOOP => Self::tfor_loop(state, ip, base, inst)?,

                // Functions (calls and returns)
                Instr::OP_CLOSURE => {
                    fixed_cost!(Instr::OP_CLOSURE);
                    state.reg_closure(frame, ra, inst.b())?;
                }
                Instr::OP_CALL => {
                    fixed_cost!(Instr::OP_CALL);
                    state.record_call_site(*ip);
                    let args = state.call_window(ra, inst.call_args())?;
                    let results = inst.call_results();
                    if crate::native_call!(
                        "run_frame->call_from_lua",
                        state.call_from_lua(args, results, inst.call_is_method(), local_cost)
                    )? {
                        return Ok(Leave::Call);
                    }
                    if let RetCount::Fixed(_) = results {
                        state.stack.resize(frame.top, Val::Nil);
                    }
                }
                Instr::OP_RETURN => {
                    fixed_cost!(Instr::OP_RETURN);
                    // No flush: a return into a Lua caller keeps the batch,
                    // and `Frame::run` settles it when the entry frame returns.
                    let count = match inst.return_count() {
                        RetCount::Fixed(n) => usize::from(n),
                        RetCount::All => state.stack.len() - ra,
                    };
                    return Ok(Leave::Return { first: ra, count });
                }
                Instr::OP_VARARG => {
                    fixed_cost!(Instr::OP_VARARG);
                    state.reg_vararg(ra, inst.vararg_count())?;
                }
                Instr::OP_TAILCALL => {
                    fixed_cost!(Instr::OP_TAILCALL);
                    // The whole call is out of line (see the function). A
                    // plain match rather than `?` keeps the unoptimized
                    // build's frame from growing a residual temporary.
                    return match crate::native_call!(
                        "run_frame->tail_call_from_lua",
                        state.tail_call_from_lua(inst, ra, *ip, local_cost)
                    ) {
                        // A Rust callee ran; its results sit in this frame's
                        // function slot upwards, which is where a return
                        // slides them, so nothing moves.
                        Ok(Some(count)) => Ok(Leave::Return {
                            first: base - 1,
                            count: usize::from(count),
                        }),
                        Ok(None) => Ok(Leave::TailCall),
                        Err(error) => Err(error),
                    };
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
    /// A Lua callee was admitted by a tail call; it replaces this frame.
    TailCall,
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
    /// a Rust function) must record first: `OP_CALL` and
    /// `OP_TFOR_CALL` in their arms (`OP_TFOR_CALL` once its `next`/`ipairs`
    /// step has declined), `OP_TAILCALL` at the start of
    /// `tail_call_from_lua`, the table and length instructions first thing in their
    /// outlined handler-call functions (`index_call`, `newindex_call`,
    /// `len_call`), so the cache hits and the resolvers that call nothing do
    /// not pay for it. The innermost
    /// record is therefore stale while its activation runs, which nothing
    /// observes: a traceback skips the innermost record (the executing frame
    /// supplies that line), and `print` reads it only from a Rust function
    /// called through one of the recorded paths. Every suspended caller's
    /// record names the instruction that suspended it. The native call
    /// inventory under reference/ lists those paths; the traceback and
    /// print-location tests pin each opcode's caller line.
    #[inline]
    pub(super) fn record_call_site(&mut self, call_site: usize) {
        if let Some(call_info) = self.in_flight.call_stack.last_mut() {
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
        match self.in_flight.open_upvalues.last() {
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

    /// Dispatch polls the interrupt only on jumps that may go backward, so
    /// admission refuses the loop exit that must not. `TFOR_LOOP` is a back
    /// edge whose taken jump polls, so it is admitted backward.
    #[test]
    fn verifier_rejects_backward_loop_exits() {
        let back_edge = Bytecode {
            code: vec![Instr::tfor_loop(0, -1), Instr::ret(0, RetCount::Fixed(0))],
            line_info: vec![1, 1],
            max_stack: 4,
            ..Bytecode::default()
        };
        crate::compiler::verify::validate_bytecode(&back_edge)
            .expect("a backward TFOR_LOOP is a polled back edge");
        let bytecode = Bytecode {
            code: vec![Instr::for_prep(0, -1), Instr::ret(0, RetCount::Fixed(0))],
            line_info: vec![1, 1],
            max_stack: 4,
            ..Bytecode::default()
        };
        let error = crate::compiler::verify::validate_bytecode(&bytecode)
            .expect_err("a backward loop exit must be rejected before dispatch");
        assert_eq!(error.instruction, Some(0));
        assert_eq!(
            error.reason,
            crate::compiler::verify::ValidationReason::BackwardLoopExit
        );
    }

    #[test]
    fn admitted_numeric_for_without_setup_returns_internal_error() {
        for code in [
            vec![Instr::for_loop(0, 0), Instr::ret(0, RetCount::Fixed(0))],
            vec![
                Instr::jump(1),
                Instr::for_prep(0, 1),
                Instr::for_loop(0, 0),
                Instr::ret(0, RetCount::Fixed(0)),
            ],
        ] {
            check_invalid_numeric_for_controls(code);
        }
    }

    #[test]
    fn admitted_numeric_for_with_overwritten_control_returns_internal_error() {
        for control in 0..3 {
            check_invalid_numeric_for_controls(vec![
                Instr::load_num(0, 0),
                Instr::load_num(1, 0),
                Instr::load_num(2, 0),
                Instr::for_prep(0, 2),
                Instr::load_bool(control, false),
                Instr::for_loop(0, -2),
                Instr::ret(0, RetCount::Fixed(0)),
            ]);
        }
    }

    fn check_invalid_numeric_for_controls(code: Vec<Instr>) {
        let bytecode = Bytecode {
            line_info: vec![1; code.len()],
            code,
            number_literals: vec![1.0],
            max_stack: 4,
            ..Bytecode::default()
        };
        crate::compiler::verify::validate_bytecode(&bytecode)
            .expect("admission checks bounds but does not prove numeric control types");
        let mut state = State::new();
        state
            .push_chunk(&Arc::new(bytecode))
            .expect("the admitted chunk fits in the state");
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("nonnumeric hidden controls must return an error without panicking");
        assert!(matches!(
            error.kind(),
            super::super::ErrorKind::InternalError(_)
        ));
        assert!(
            error
                .to_string()
                .contains("numeric for controls must remain numbers")
        );
        assert!(!state.is_poisoned());
        let program =
            crate::Program::compile("return", None).expect("the recovery chunk is valid Lua");
        state.load(&program).expect("the state remains loadable");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("the error boundary leaves the state callable");
    }

    /// Admission does not prove that a call, tail call or open vararg window
    /// starts above every captured local (see `validate_bytecode`), so
    /// `check_window` refuses it at run time. Each case captures register
    /// 0 (the window's first slot) or 1 (strictly inside it), publishes the
    /// capturing closure as `f`, then opens the window at register 0.
    #[test]
    fn admitted_window_below_an_open_upvalue_returns_internal_error() {
        let windows: [&[Instr]; 3] = [
            &[Instr::call(
                0,
                ArgCount::Fixed(0),
                RetCount::Fixed(0),
                false,
            )],
            &[
                Instr::call(0, ArgCount::Fixed(0), RetCount::Fixed(0), false).into_tail_call(),
                Instr::ret(0, RetCount::Fixed(0)),
            ],
            &[
                Instr::vararg(0, RetCount::All),
                Instr::ret(0, RetCount::All),
            ],
        ];
        for window in windows {
            for captured in 0..2u8 {
                check_window_below_open_upvalue(window, captured);
            }
        }
    }

    fn check_window_below_open_upvalue(window: &[Instr], captured: u8) {
        use crate::compiler::UpvalueDesc;
        let child = Bytecode {
            code: vec![Instr::get_upval(0, 0), Instr::ret(0, RetCount::Fixed(1))],
            line_info: vec![1; 2],
            max_stack: 1,
            upvalues: vec![UpvalueDesc::Local(captured)],
            ..Bytecode::default()
        };
        let mut code = vec![
            Instr::load_num(u16::from(captured), 0),
            Instr::closure(2, 0),
            Instr::set_global(2, 0, None),
        ];
        code.extend_from_slice(window);
        code.push(Instr::ret(0, RetCount::Fixed(0)));
        let bytecode = Bytecode {
            line_info: vec![1; code.len()],
            code,
            number_literals: vec![7.0],
            string_literals: vec![b"f".to_vec()],
            max_stack: 4,
            is_vararg: true,
            nested: vec![Arc::new(child)],
            ..Bytecode::default()
        };
        crate::compiler::verify::validate_bytecode(&bytecode)
            .expect("admission does not prove windows start above captured locals");
        let mut state = State::new();
        state
            .push_chunk(&Arc::new(bytecode))
            .expect("the admitted chunk fits in the state");
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("a window over an open upvalue must return an error without panicking");
        assert!(matches!(
            error.kind(),
            super::super::ErrorKind::InternalError(_)
        ));
        assert!(
            error
                .to_string()
                .contains("a register window overlaps an open upvalue"),
            "{error}"
        );
        assert!(!state.is_poisoned());
        assert!(state.in_flight.open_upvalues.is_empty());
        // The unwind closed the capture with the value it held.
        state.get_global("f").expect("the closure was published");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(1))
            .expect("the error boundary leaves the state callable");
        assert_eq!(state.to_number(-1).expect("the capture is a number"), 7.0);
    }

    #[test]
    fn documented_opcode_charges_match_static_costs() {
        use crate::cost_model::CostModel;
        use crate::instr::OpcodeCharge;
        let docs = include_str!("../../docs/budget.md");
        let section = docs
            .split_once("## Static opcode charges")
            .map(|(_, rest)| rest)
            .and_then(|rest| rest.split_once("\n## ").map(|(section, _)| section))
            .expect("budget documentation contains the opcode charge table");
        let mut keys = Vec::new();
        let model = CostModel::default();

        for opcode in u8::MIN..=u8::MAX {
            if !Instr::known_opcode(opcode) {
                continue;
            }

            let charging = Instr::charging(opcode);
            let (key, operation) = match charging {
                OpcodeCharge::Free => ("none".to_string(), String::new()),
                OpcodeCharge::Once(charge) | OpcodeCharge::PerElement(charge) => {
                    (format!("`{charge:?}`"), format!("`{}`", charge.name()))
                }
            };
            let row_prefix = format!("| {key} |");
            let row = section
                .lines()
                .find(|line| line.starts_with(&row_prefix))
                .expect("each opcode charge has a documented row");
            if !keys.contains(&key) {
                keys.push(key.clone());
            }

            let columns: Vec<&str> = row.split('|').map(str::trim).collect();
            assert_eq!(columns[2], operation, "{key}");
            let documented_units = columns[3];
            match charging {
                OpcodeCharge::PerElement(charge) => {
                    assert_eq!(documented_units, "encoded count");
                    assert_eq!(opcode, Instr::OP_SET_LIST);
                    let units = Instr::set_list(0, Some(7), 0).static_units();
                    assert_eq!(units, Some((charge, 7)));
                    assert_eq!(model.cost(charge, 7), 7);
                }
                OpcodeCharge::Once(_) | OpcodeCharge::Free => {
                    let documented_units = documented_units
                        .parse::<u64>()
                        .expect("fixed opcode charge is a unit count");
                    let fixed =
                        Instr::fixed_charge(opcode).map_or(0, |charge| model.cost(charge, 1));
                    assert_eq!(fixed, documented_units, "{key}");
                }
            }
        }

        // Every table row except the header and the separator is a charge.
        let documented_rows = section
            .lines()
            .filter(|line| line.starts_with("| ") && !line.starts_with("| Charge |"))
            .filter(|line| !line.starts_with("| ---"))
            .count();
        assert_eq!(documented_rows, keys.len());
        assert_eq!(
            Instr::set_list(0, None, 0).static_units(),
            Some((Instr::SET_LIST_ELEMENT_CHARGE, 0))
        );
    }
}
