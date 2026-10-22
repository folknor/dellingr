use std::sync::Arc;

// The constant folder and runtime dispatch use the arithmetic functions
// declared with each opcode in `instr.rs`.
use crate::numeral::{lua_add, lua_div, lua_modulo, lua_mul, lua_sub};

use super::super::error::{Error, ErrorKind, StackFrame};
use super::Bytecode;
use super::BytecodeRuntime;
use super::Instr;
use super::Result;
use super::State;
use super::Val;
use super::object::UpvalueRef;
use crate::instr::{ArgCount, RetCount};
use crate::limits::tuning;

/// Stack positions pushed by `OP_MARK_CALL_BASE` and `OP_NEW_TABLE_TRACKED`
/// and awaiting their Dynamic `OP_CALL` or `SetList(0)`.
///
/// The vectors are shared by every active Lua frame so that no frame
/// allocates, but each entry belongs to the frame that pushed it: a frame
/// records a [`PendingFloor`] on entry, reads and pops only entries above it,
/// and truncates back to it on exit. A frame therefore cannot consume a
/// caller's base, and an error between a push and its consumer leaves nothing
/// behind once the frame unwinds. Host code never sees these entries.
#[derive(Debug, Default)]
pub(super) struct PendingBases {
    calls: Vec<usize>,
    constructors: Vec<usize>,
}

/// One frame's view boundary into [`PendingBases`].
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct PendingFloor {
    calls: usize,
    constructors: usize,
}

impl PendingBases {
    pub(super) fn floor(&self) -> PendingFloor {
        PendingFloor {
            calls: self.calls.len(),
            constructors: self.constructors.len(),
        }
    }

    /// Discard every entry the frame owning `floor` pushed.
    pub(super) fn truncate(&mut self, floor: PendingFloor) {
        self.calls.truncate(floor.calls);
        self.constructors.truncate(floor.constructors);
    }

    #[cfg(feature = "snapshot")]
    pub(super) fn is_empty(&self) -> bool {
        self.calls.is_empty() && self.constructors.is_empty()
    }

    #[cfg(feature = "snapshot")]
    pub(super) fn clear(&mut self) {
        self.calls.clear();
        self.constructors.clear();
    }
}

/// A `Frame` represents a single stack-frame of a Lua function.
pub(super) struct Frame {
    /// The bytecode being executed (shared via Arc; cheap to clone, immutable).
    bytecode: Arc<Bytecode>,
    /// State-local literals and lookup caches shared by this bytecode's
    /// closures and active frames.
    pub(super) runtime: Arc<BytecodeRuntime>,
    /// The index of the next (not current) instruction
    ip: usize,
    /// The upvalues captured by this closure. Shared with the `Closure`
    /// (and any other active frames on it) - never mutated through the
    /// frame; writes go through the `UpvaluePool` slots.
    pub(super) upvalues: Arc<[UpvalueRef]>,
    /// The varargs passed to this function (if it's a vararg function).
    varargs: Vec<Val>,
    /// Lengths of `State::pending_bases` when this frame started; entries
    /// below belong to callers.
    pub(super) pending_floor: PendingFloor,
    /// The stack bottom when this frame was created (used for closing upvalues).
    pub(super) stack_bottom: usize,
}

impl Frame {
    /// Create a frame from bytecode admitted by compilation or snapshot load.
    /// No verification is repeated here: this runs on every recursive call.
    #[must_use]
    pub(super) fn new(
        bytecode: Arc<Bytecode>,
        runtime: Arc<BytecodeRuntime>,
        upvalues: Arc<[UpvalueRef]>,
        varargs: Vec<Val>,
        stack_bottom: usize,
        pending_floor: PendingFloor,
    ) -> Self {
        let ip = 0;
        Self {
            bytecode,
            runtime,
            ip,
            upvalues,
            varargs,
            pending_floor,
            stack_bottom,
        }
    }

    /// This frame's innermost pending constructor base, if it pushed one.
    pub(super) fn constructor_base(&self, state: &State) -> Option<usize> {
        state
            .pending_bases
            .constructors
            .get(self.pending_floor.constructors..)?
            .last()
            .copied()
    }

    /// Pop this frame's innermost pending constructor base. Verified marker
    /// pairing guarantees the top marker belongs to this frame, never a caller.
    pub(super) fn pop_constructor_base(&self, state: &mut State) {
        debug_assert!(
            state.pending_bases.constructors.len() > self.pending_floor.constructors,
            "set-list would pop a caller's constructor marker"
        );
        let _base = state
            .pending_bases
            .constructors
            .pop()
            .expect("verified set-list consumes its frame's constructor marker");
    }

    fn call_args(&self, state: &mut State, args: ArgCount) -> Result<u8> {
        let actual = match args {
            ArgCount::Fixed(actual) => actual,
            ArgCount::Dynamic => {
                let calls = &mut state.pending_bases.calls;
                let owned = if calls.len() > self.pending_floor.calls {
                    calls.pop()
                } else {
                    None
                };
                let base = owned.expect("verified dynamic call owns a pending base");
                let actual_len = state.stack.len() - base - 1;
                u8::try_from(actual_len).map_err(|_| {
                    state.error(ErrorKind::TooManyArguments {
                        limit: crate::limits::MAX_ARGUMENTS,
                    })
                })?
            }
        };
        Ok(actual)
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
        let Some(instruction) = self.ip.checked_sub(1) else {
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
        let idx = self.ip.saturating_sub(1);
        self.bytecode.line_info.get(idx).copied().unwrap_or(0)
    }

    /// Record the instruction currently dispatching a call into the active
    /// call-stack entry, so outer traceback frames name the right call site.
    /// Every opcode that can enter Lua through a call or metamethod must record
    /// its instruction here before handing off; the regression test pins those
    /// caller lines.
    #[inline]
    fn record_call_site(&self, state: &mut State) {
        if let Some(call_info) = state.call_stack.last_mut() {
            call_info.ip = self.ip;
        }
    }

    /// Create a StackFrame for error reporting.
    pub(super) fn to_stack_frame(&self) -> StackFrame {
        StackFrame {
            function_name: self.bytecode.name.clone(),
            source: self.bytecode.source.clone(),
            line: self.current_line(),
        }
    }

    // Static bytecode contracts are checked at compile/load admission, not
    // again during dispatch. Like literal/local indexing and stack pops, a
    // verifier regression here is a VM bug and may panic. Runtime values,
    // host API inputs and resource limits still require fallible checks.
    /// Jump forward/back by a verified `offset` in instructions.
    pub(super) fn jump(&mut self, offset: i16) {
        self.ip = if offset >= 0 {
            self.ip + offset as usize
        } else {
            self.ip - offset.unsigned_abs() as usize
        };
    }

    /// Get the instruction at the instruction pointer, and advance the
    /// instruction pointer accordingly.
    fn get_instr(&mut self) -> Instr {
        let i = self.bytecode.code[self.ip];
        self.ip += 1;
        i
    }

    #[must_use]
    pub(super) fn get_nested_bytecode(&mut self, i: u8) -> Arc<Bytecode> {
        Arc::clone(&self.bytecode.nested[i as usize])
    }

    #[must_use]
    fn get_number_constant(&self, i: u16) -> f64 {
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

    /// Start evaluating instructions from the current position.
    ///
    /// Fixed opcode charges come from `Instr::static_units`. SET_LIST charges
    /// its live element count (the analyzer only knows its encoded prefix).
    /// Calls and access helpers can additionally charge native work, including
    /// strings, patterns, table traversal and metamethod callbacks.
    pub(super) fn eval(&mut self, state: &mut State) -> Result<RetCount> {
        // Batch cost checking: accumulate locally and flush periodically
        let mut pending: u64 = 0;
        let result = self.eval_batched(state, &mut pending);
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

    /// Dispatch loop body for [`Frame::eval`]. `local_cost` holds accrued but
    /// not yet flushed opcode cost; the caller settles it on error.
    #[inline(always)]
    fn eval_batched(&mut self, state: &mut State, local_cost: &mut u64) -> Result<RetCount> {
        /// Macro to accumulate cost and flush when threshold is reached
        macro_rules! add_cost {
            ($state:expr, $local:expr, $cost:expr) => {{
                let next = (*$local).saturating_add($cost);
                // This decides when to flush, not whether to accept a charge.
                // consume_cost owns the runtime meter's admission rule. A
                // finite budget also flushes before pending cost would exhaust
                // it, so the interval does not change the charged prefix or
                // `cost_used` at `BudgetExceeded`; any other error settles the
                // pending batch in `Frame::eval`.
                if next >= tuning::COST_CHECK_INTERVAL || $state.cost_batch_needs_flush(next) {
                    $state.charge_cost(next)?;
                    *$local = 0;
                } else {
                    *$local = next;
                }
            }};
        }

        // Match arms cannot come from a macro, so the opcode rows feed two
        // expansions: the or-pattern selecting every arithmetic opcode, and
        // the inner match giving each one its direct function-item call.
        macro_rules! arithmetic_opcode_pattern {
            (@ [$($names:ident)*];
                $name:ident = $number:literal, $reserved:literal, $transfer:literal,
                    $cost:ident [$arithmetic:path, $foldable:literal], $debug:expr;
                $($rest:tt)*) => {
                arithmetic_opcode_pattern!(@ [$($names)* $name]; $($rest)*)
            };
            (@ [$($names:ident)*];
                $name:ident = $number:literal, $reserved:literal, $transfer:literal,
                    $cost:ident, $debug:expr;
                $($rest:tt)*) => {
                arithmetic_opcode_pattern!(@ [$($names)*]; $($rest)*)
            };
            (@ [$($names:ident)+];) => {
                $(Instr::$names)|+
            };
            (; $($rows:tt)*) => {
                arithmetic_opcode_pattern!(@ []; $($rows)*)
            };
        }

        macro_rules! arithmetic_dispatch {
            (@ $state:ident, $local:ident, $opcode:ident; [$($names:ident => $fns:path,)*];
                $name:ident = $number:literal, $reserved:literal, $transfer:literal,
                    $cost:ident [$arithmetic:path, $foldable:literal], $debug:expr;
                $($rest:tt)*) => {
                arithmetic_dispatch!(
                    @ $state, $local, $opcode; [$($names => $fns,)* $name => $arithmetic,];
                    $($rest)*
                )
            };
            (@ $state:ident, $local:ident, $opcode:ident; [$($names:ident => $fns:path,)*];
                $name:ident = $number:literal, $reserved:literal, $transfer:literal,
                    $cost:ident, $debug:expr;
                $($rest:tt)*) => {
                arithmetic_dispatch!(@ $state, $local, $opcode; [$($names => $fns,)*]; $($rest)*)
            };
            (@ $state:ident, $local:ident, $opcode:ident; [$($names:ident => $fns:path,)+];) => {
                match $opcode.opcode() {
                    $(
                        Instr::$names => {
                            add_cost!(
                                $state,
                                $local,
                                const { Instr::op(Instr::$names).static_units() }
                            );
                            $state.eval_float_float($fns)?;
                        }
                    )+
                    _ => unreachable!("arithmetic dispatch received a non-arithmetic opcode"),
                }
            };
            ($state:ident, $local:ident, $opcode:ident; $($rows:tt)*) => {
                arithmetic_dispatch!(@ $state, $local, $opcode; []; $($rows)*)
            };
        }

        loop {
            let inst = self.get_instr();
            match inst.opcode() {
                // === Dispatch operations ===

                // General control flow
                Instr::OP_NOP => {}
                Instr::OP_POP => {
                    state.pop_val();
                }
                Instr::OP_DUP => {
                    let val = *state
                        .stack
                        .last()
                        .expect("Dup instruction requires a stack value");
                    state.push_val(val)?;
                }
                Instr::OP_SWAP => {
                    let len = state.stack.len();
                    state.stack.swap(len - 1, len - 2);
                }
                Instr::OP_JUMP => self.jump(inst.sbx()),
                Instr::OP_BRANCH_FALSE => state.instr_branch(self, false, inst.sbx(), false)?,
                Instr::OP_BRANCH_FALSE_KEEP => state.instr_branch(self, false, inst.sbx(), true)?,
                Instr::OP_BRANCH_TRUE_KEEP => state.instr_branch(self, true, inst.sbx(), true)?,

                // Local variables
                Instr::OP_GET_LOCAL => state.instr_get_local(inst.a())?,
                Instr::OP_SET_LOCAL => state.instr_set_local(inst.a()),

                // Upvalues
                Instr::OP_GET_UPVALUE => state.instr_get_upvalue(self, inst.a())?,
                Instr::OP_SET_UPVALUE => state.instr_set_upvalue(self, inst.a()),

                // Globals
                Instr::OP_GET_GLOBAL => state.instr_get_global(self, inst.bx(), inst.a())?,
                Instr::OP_SET_GLOBAL => state.instr_set_global(self, inst.bx(), inst.a())?,

                // Builtins (fast path for well-known globals)
                Instr::OP_GET_BUILTIN => state.instr_get_builtin(inst.a())?,
                Instr::OP_SET_BUILTIN => state.instr_set_builtin(inst.a()),

                // Functions (calls and returns)
                Instr::OP_CLOSURE => state.instr_closure(self, inst.a())?,
                Instr::OP_CALL => {
                    self.record_call_site(state);
                    Self::flush_local_cost(state, local_cost)?;
                    let args = self.call_args(state, ArgCount::from_u8(inst.a()))?;
                    state.call_op(args, RetCount::from_u8(inst.b()))?;
                }
                Instr::OP_MARK_CALL_BASE => {
                    // Verification proves a callee exists above this frame's
                    // floor and the fixed adjustment is one slot.
                    let base = state.stack.len() - 1;
                    state.pending_bases.calls.push(base);
                }
                Instr::OP_CLOSE_UPVALUES => {
                    // Close upvalues for locals at or above the given slot
                    // Used at end of loop iterations to capture per-iteration locals
                    let stack_level = state.stack_bottom + inst.a() as usize;
                    state.close_upvalues(stack_level);
                }
                Instr::OP_RETURN => {
                    // Flush any remaining accumulated cost before returning
                    Self::flush_local_cost(state, local_cost)?;
                    return Ok(RetCount::from_u8(inst.a()));
                }
                Instr::OP_VARARG => {
                    let n = inst.a();
                    if n == u8::MAX {
                        // Push all varargs
                        state.check_stack_space(self.varargs.len())?;
                        for val in &self.varargs {
                            state.push_unchecked(*val);
                        }
                    } else {
                        // Push exactly n values, padding with nil if needed
                        let n = n as usize;
                        state.check_stack_space(n)?;
                        for i in 0..n {
                            if i < self.varargs.len() {
                                state.push_unchecked(self.varargs[i]);
                            } else {
                                state.push_unchecked(Val::Nil);
                            }
                        }
                    }
                }

                // Literals
                Instr::OP_PUSH_NIL => state.push_val(Val::Nil)?,
                Instr::OP_PUSH_BOOL => state.push_val(Val::Bool(inst.a() != 0))?,
                Instr::OP_PUSH_NUM => {
                    let n = self.get_number_constant(inst.bx());
                    state.push_val(Val::Num(n))?;
                }
                Instr::OP_PUSH_STRING => {
                    let val = self.literal(inst.bx());
                    state.push_val(val)?;
                }

                // Equality
                Instr::OP_EQUAL => {
                    let val2 = state.pop_val();
                    let val1 = state.pop_val();
                    state.push_unchecked(Val::Bool(val1 == val2));
                }
                Instr::OP_NOT_EQUAL => {
                    let val2 = state.pop_val();
                    let val1 = state.pop_val();
                    state.push_unchecked(Val::Bool(val1 != val2));
                }

                // Orderings
                // Supports both number and string comparisons
                Instr::OP_LESS => state.eval_compare(Instr::LESS_ORDER.0, Instr::LESS_ORDER.1)?,
                Instr::OP_GREATER => {
                    state.eval_compare(Instr::GREATER_ORDER.0, Instr::GREATER_ORDER.1)?;
                }
                Instr::OP_LESS_EQUAL => {
                    state.eval_compare(Instr::LESS_EQUAL_ORDER.0, Instr::LESS_EQUAL_ORDER.1)?;
                }
                Instr::OP_GREATER_EQUAL => state
                    .eval_compare(Instr::GREATER_EQUAL_ORDER.0, Instr::GREATER_EQUAL_ORDER.1)?,

                // Fused comparison + BranchFalse: pop two
                // operands, jump when the comparison is false. One range arm
                // with the variant decode outlined - per-variant arms here
                // measurably perturbed the dispatch loop's codegen.
                Instr::OP_BRANCH_FALSE_LESS..=Instr::OP_BRANCH_FALSE_NOT_EQUAL => {
                    state.instr_branch_fused(self, inst.opcode(), inst.sbx())?;
                }

                // `for` loops
                Instr::OP_FOR_LOOP => state.instr_for_loop(self, inst.a(), inst.sbx())?,
                Instr::OP_FOR_PREP => state.instr_for_prep(self, inst.a(), inst.sbx())?,

                // Generic `for` loops
                Instr::OP_TFOR_PREP => state.instr_tfor_prep(inst.a()),
                Instr::OP_TFOR_CALL => {
                    self.record_call_site(state);
                    Self::flush_local_cost(state, local_cost)?;
                    state.instr_tfor_call(inst.a(), inst.b(), inst.c(), &self.runtime.caches)?;
                }
                Instr::OP_TFOR_LOOP => state.instr_tfor_loop(self, inst.a(), inst.sbx())?,

                // Length helpers may charge native work.
                Instr::OP_LENGTH => {
                    self.record_call_site(state);
                    state.instr_length(local_cost)?;
                }

                // Logical not
                Instr::OP_NOT => state.instr_not(),

                // Table reads
                Instr::OP_GET_FIELD => {
                    self.record_call_site(state);
                    state.instr_get_field(self, inst.bx(), inst.a(), local_cost)?;
                }
                Instr::OP_GET_TABLE => {
                    self.record_call_site(state);
                    state.instr_get_table(local_cost)?;
                }

                // Operand A is the chain length (>= 2). Chained `..` collapses
                // into one OP_CONCAT so a long concat has only one intermediate
                // allocation.
                //
                // The bytes the operator emits are deliberately uncharged: the
                // budget is a game-design instrument rather than a CPU meter
                // (docs/budget.md), and `..` is structural syntax that a
                // legible refactor must not make more expensive. The size is
                // still bounded by `checked_string_growth` (MAX_STRING_BYTES),
                // so the uncharged work is capped. Library calls that emit
                // bytes (`table.concat`, `string.format`, ...) charge them;
                // the operator does not.
                Instr::OP_CONCAT => state.concat_helper(inst.a() as usize)?,

                // === Fixed-charge operations ===

                // Arithmetic expands from the opcode rows, keeping each
                // operation a direct call to its function item.
                crate::instr::opcode_rows!(arithmetic_opcode_pattern, (), inst, f) => {
                    crate::instr::opcode_rows!(
                        arithmetic_dispatch,
                        (state, local_cost, inst),
                        inst,
                        f
                    );
                }

                // Unary negation
                Instr::OP_NEGATE => {
                    add_cost!(
                        state,
                        local_cost,
                        const { Instr::op(Instr::OP_NEGATE).static_units() }
                    );
                    state.instr_negate()?;
                }

                // Table creation
                Instr::OP_NEW_TABLE => {
                    add_cost!(
                        state,
                        local_cost,
                        const { Instr::op(Instr::OP_NEW_TABLE).static_units() }
                    );
                    state.push_new_table()?;
                }
                Instr::OP_NEW_TABLE_PRESIZED => {
                    add_cost!(
                        state,
                        local_cost,
                        const { Instr::op(Instr::OP_NEW_TABLE_PRESIZED).static_units() }
                    );
                    // A is a hash-capacity hint: the parser encodes the
                    // constructor's keyed-field count. The array part is
                    // reserved per SET_LIST batch instead. Bytecode from older
                    // compilers encoded the total field count; it is still a
                    // valid (oversized) hint.
                    state.new_table_with_capacity(inst.a() as usize)?;
                }
                Instr::OP_NEW_TABLE_TEMPLATE => {
                    add_cost!(
                        state,
                        local_cost,
                        const { Instr::op(Instr::OP_NEW_TABLE_TEMPLATE).static_units() }
                    );
                    state.instr_new_table_template(self, inst.a())?;
                }
                Instr::OP_NEW_TABLE_TRACKED => {
                    add_cost!(
                        state,
                        local_cost,
                        const { Instr::op(Instr::OP_NEW_TABLE_TRACKED).static_units() }
                    );
                    let table_idx = state.stack.len();
                    state.new_table_with_capacity(inst.a() as usize)?;
                    state.pending_bases.constructors.push(table_idx);
                }

                // Table writes
                Instr::OP_INIT_FIELD => {
                    add_cost!(
                        state,
                        local_cost,
                        const { Instr::op(Instr::OP_INIT_FIELD).static_units() }
                    );
                    state.instr_init_field(self, inst.a(), inst.bx())?;
                }
                Instr::OP_INIT_FIELD_PINNED => {
                    add_cost!(
                        state,
                        local_cost,
                        const { Instr::op(Instr::OP_INIT_FIELD_PINNED).static_units() }
                    );
                    state.instr_init_field_pinned(self, inst.bx(), inst.a())?;
                }
                Instr::OP_INIT_INDEX => {
                    add_cost!(
                        state,
                        local_cost,
                        const { Instr::op(Instr::OP_INIT_INDEX).static_units() }
                    );
                    state.instr_init_index(inst.a())?;
                }
                Instr::OP_SET_FIELD => {
                    self.record_call_site(state);
                    add_cost!(
                        state,
                        local_cost,
                        const { Instr::op(Instr::OP_SET_FIELD).static_units() }
                    );
                    state.instr_set_field(self, 0, inst.bx(), inst.a(), local_cost)?;
                }
                Instr::OP_SET_FIELD_AT => {
                    self.record_call_site(state);
                    add_cost!(
                        state,
                        local_cost,
                        const { Instr::op(Instr::OP_SET_FIELD_AT).static_units() }
                    );
                    state.instr_set_field(self, inst.a(), inst.bx(), u8::MAX, local_cost)?;
                }
                Instr::OP_SET_TABLE => {
                    self.record_call_site(state);
                    add_cost!(
                        state,
                        local_cost,
                        const { Instr::op(Instr::OP_SET_TABLE).static_units() }
                    );
                    state.instr_set_table(inst.a(), local_cost)?;
                }

                // Array initialization
                Instr::OP_SET_LIST => {
                    let n = inst.a();
                    let count = state.instr_set_list_count(self, n)?;
                    add_cost!(state, local_cost, count as u64);
                    state.instr_set_list(self, n, inst.bx())?;
                }

                // Admission rejects unknown opcodes, including dead code.
                _ => unreachable!("verified bytecode contains an unknown opcode"),
            }
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
    fn jump_accepts_i16_minimum_offset() {
        let bytecode = Arc::new(Bytecode {
            code: vec![Instr::ret(RetCount::Fixed(0)); i16::MIN.unsigned_abs() as usize],
            ..Bytecode::default()
        });
        let runtime = Arc::new(BytecodeRuntime {
            literals: Box::new([]),
            caches: crate::vm::caches::RuntimeCaches::new(&bytecode),
        });
        let mut frame = Frame::new(
            bytecode,
            runtime,
            Arc::from([]),
            Vec::new(),
            0,
            PendingFloor::default(),
        );
        frame.ip = i16::MIN.unsigned_abs() as usize;

        frame.jump(i16::MIN);
        assert_eq!(frame.ip, 0);
    }

    #[test]
    fn verifier_rejects_end_of_bytecode_jump() {
        let bytecode = Bytecode {
            code: vec![
                Instr::op_a_sbx(Instr::OP_JUMP, 0, 1),
                Instr::ret(RetCount::Fixed(0)),
            ],
            line_info: vec![1, 1],
            ..Bytecode::default()
        };
        let error = crate::compiler::verify::validate_bytecode(&bytecode)
            .expect_err("jump to end must be rejected before dispatch");
        assert_eq!(error.instruction, Some(0));
        assert_eq!(error.reason, "jump target is out of range");
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
                assert_eq!(documented_units, "encoded `A`");
                assert_eq!(Instr::op_a(opcode, 7).static_units(), 7);
            } else {
                let documented_units = documented_units
                    .parse::<u64>()
                    .expect("fixed opcode charge is a unit count");
                assert_eq!(
                    Instr::op(opcode).static_units(),
                    documented_units,
                    "{class_name}"
                );
            }
        }

        let documented_rows = section
            .lines()
            .filter(|line| line.starts_with("| `"))
            .count();
        assert_eq!(documented_rows, classes.len() + 1);
        assert_eq!(Instr::set_list(0).static_units(), 0);
    }

    #[test]
    fn reentry_accounting_sources_cover_the_intended_dispatch_opcodes() {
        let cases = [
            ("callable()", Instr::OP_CALL, None, "CALL"),
            (
                "for item in iterator do end",
                Instr::OP_TFOR_CALL,
                None,
                "TFOR_CALL",
            ),
            ("local result = #lengthy", Instr::OP_LENGTH, None, "LENGTH"),
            (
                "local result = indexed.field",
                Instr::OP_GET_FIELD,
                None,
                "GET_FIELD",
            ),
            (
                "local result = indexed[left]",
                Instr::OP_GET_TABLE,
                None,
                "GET_TABLE",
            ),
            (
                "new_indexed.field = right",
                Instr::OP_SET_FIELD,
                None,
                "SET_FIELD",
            ),
            (
                "other.field, new_indexed.field = left, right",
                Instr::OP_SET_FIELD_AT,
                Some(1),
                "SET_FIELD_AT",
            ),
            (
                "new_indexed[left] = right",
                Instr::OP_SET_TABLE,
                None,
                "SET_TABLE",
            ),
        ];

        for (source, expected_opcode, expected_a, name) in cases {
            let bytecode = crate::compiler::parse_str(source)
                .unwrap_or_else(|error| panic!("{name} source compiles: {error}"));
            assert!(
                bytecode.code.iter().any(|instruction| {
                    instruction.opcode() == expected_opcode
                        && expected_a.is_none_or(|a| instruction.a() == a)
                }),
                "{name} source must exercise its dispatch opcode"
            );
        }
    }
}
