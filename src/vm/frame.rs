use std::ops;

use crate::numeral::lua_modulo;
use std::sync::Arc;

use super::super::error::{Error, ErrorKind, StackFrame};
use super::Bytecode;
use super::BytecodeRuntime;
use super::Instr;
use super::Result;
use super::State;
use super::Val;
use super::object::UpvalueRef;
use crate::instr::{ArgCount, RetCount};

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
    /// The stack bottom when this frame was created (used for closing upvalues).
    pub(super) stack_bottom: usize,
}

impl Frame {
    /// Create a new Frame.
    #[must_use]
    pub(super) fn new(
        bytecode: Arc<Bytecode>,
        runtime: Arc<BytecodeRuntime>,
        upvalues: Arc<[UpvalueRef]>,
        varargs: Vec<Val>,
        stack_bottom: usize,
    ) -> Self {
        let ip = 0;
        Self {
            bytecode,
            runtime,
            ip,
            upvalues,
            varargs,
            stack_bottom,
        }
    }

    /// Get the bytecode being executed.
    pub(super) fn bytecode(&self) -> &Arc<Bytecode> {
        &self.bytecode
    }

    pub(super) fn literal(&self, i: u16) -> Val {
        self.runtime.literals[i as usize]
    }

    /// Get the current line number (1-indexed), or 0 if unknown.
    pub(super) fn current_line(&self) -> u32 {
        // ip points to the NEXT instruction, so use ip-1 for current
        let idx = self.ip.saturating_sub(1);
        self.bytecode.line_info.get(idx).copied().unwrap_or(0)
    }

    /// Record the instruction currently dispatching a call into the active
    /// call-stack entry, so outer traceback frames name the right call site.
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

    /// Jump forward/back by `offset` instructions.
    pub(super) fn jump(&mut self, offset: i16) -> Result<()> {
        let new_ip = if offset >= 0 {
            self.ip.checked_add(offset as usize)
        } else {
            self.ip.checked_sub(offset.unsigned_abs() as usize)
        };
        match new_ip {
            Some(ip) if ip < self.bytecode.code.len() => {
                self.ip = ip;
                Ok(())
            }
            _ => Err(Error::without_location(ErrorKind::InvalidJump {
                ip: self.ip,
                offset: offset as isize,
            })),
        }
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

    /// How often to flush accumulated cost to the state.
    /// Higher values reduce overhead but may overshoot budget more.
    const COST_CHECK_INTERVAL: u64 = 64;

    pub(super) fn flush_local_cost(state: &mut State, local_cost: &mut u64) -> Result<()> {
        if *local_cost > 0 {
            state.consume_cost(*local_cost)?;
            *local_cost = 0;
        }
        Ok(())
    }

    /// Start evaluating instructions from the current position.
    ///
    /// Cost system: Most operations are free. Only arithmetic, table writes,
    /// and table creation cost points. This rewards thoughtful code organization
    /// while ensuring every script can do meaningful work.
    ///
    /// Free operations: control flow, variable access, comparisons, function calls,
    /// table reads, string operations, length operator.
    ///
    /// Costs 1: arithmetic (+, -, *, /, %, ^, unary -), table creation,
    /// table writes (including array initialization).
    pub(super) fn eval(&mut self, state: &mut State) -> Result<RetCount> {
        // Batch cost checking: accumulate locally and flush periodically
        let mut local_cost: u64 = 0;

        /// Macro to accumulate cost and flush when threshold is reached
        macro_rules! add_cost {
            ($state:expr, $local:expr, $cost:expr) => {{
                let next = $local.saturating_add($cost);
                if next >= Self::COST_CHECK_INTERVAL
                    || $state.cost_remaining().saturating_sub_unsigned(next) <= 0
                {
                    $state.consume_cost(next)?;
                    $local = 0;
                } else {
                    $local = next;
                }
            }};
        }

        loop {
            let inst = self.get_instr();
            #[cfg(feature = "debug_vm")]
            println!("{inst:?}");
            match inst.opcode() {
                // === FREE OPERATIONS (cost 0) ===

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
                Instr::OP_JUMP => self.jump(inst.sbx())?,
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

                // Functions (calls and returns are free)
                Instr::OP_CLOSURE => state.instr_closure(self, inst.a())?,
                Instr::OP_CALL => {
                    self.record_call_site(state);
                    Self::flush_local_cost(state, &mut local_cost)?;
                    state.call(ArgCount::from_u8(inst.a()), RetCount::from_u8(inst.b()))?;
                }
                Instr::OP_MARK_CALL_BASE => {
                    let adjustment = inst.a() as usize;
                    // Validate against the CURRENT FRAME, not just against
                    // usize underflow. Checking only `stack.len()` would let a
                    // marker whose base lands below `stack_bottom` through
                    // whenever the absolute stack happens to be deep enough,
                    // and the dynamic-call path would then treat a caller-owned
                    // slot as this call's callee.
                    let base = state
                        .stack
                        .len()
                        .checked_sub(adjustment)
                        .filter(|base| *base >= state.stack_bottom)
                        .ok_or_else(|| {
                            state.error(ErrorKind::InternalError(
                                "call-base marker is below the active frame".into(),
                            ))
                        })?;
                    state.vararg_call_bases.push(base);
                }
                Instr::OP_CLOSE_UPVALUES => {
                    // Close upvalues for locals at or above the given slot
                    // Used at end of loop iterations to capture per-iteration locals
                    let stack_level = state.stack_bottom + inst.a() as usize;
                    state.close_upvalues(stack_level);
                }
                Instr::OP_RETURN => {
                    // Flush any remaining accumulated cost before returning
                    Self::flush_local_cost(state, &mut local_cost)?;
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

                // Literals (free)
                Instr::OP_PUSH_NIL => state.push_nil()?,
                Instr::OP_PUSH_BOOL => state.push_boolean(inst.a() != 0)?,
                Instr::OP_PUSH_NUM => {
                    let n = self.get_number_constant(inst.bx());
                    state.push_number(n)?;
                }
                Instr::OP_PUSH_STRING => {
                    let val = state.get_string_constant(self, inst.bx());
                    state.push_val(val)?;
                }

                // Equality (comparisons are free)
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

                // Orderings (comparisons are free)
                // Supports both number and string comparisons
                Instr::OP_LESS => state.eval_compare(std::cmp::Ordering::Less, false)?,
                Instr::OP_GREATER => state.eval_compare(std::cmp::Ordering::Greater, false)?,
                Instr::OP_LESS_EQUAL => state.eval_compare(std::cmp::Ordering::Greater, true)?, // <= is !>
                Instr::OP_GREATER_EQUAL => state.eval_compare(std::cmp::Ordering::Less, true)?, // >= is !<

                // Fused comparison + BranchFalse (both halves free): pop two
                // operands, jump when the comparison is false.
                Instr::OP_BRANCH_FALSE_LESS => {
                    if !state.eval_compare_bool(std::cmp::Ordering::Less, false)? {
                        self.jump(inst.sbx())?;
                    }
                }
                Instr::OP_BRANCH_FALSE_GREATER => {
                    if !state.eval_compare_bool(std::cmp::Ordering::Greater, false)? {
                        self.jump(inst.sbx())?;
                    }
                }
                Instr::OP_BRANCH_FALSE_LESS_EQUAL => {
                    if !state.eval_compare_bool(std::cmp::Ordering::Greater, true)? {
                        self.jump(inst.sbx())?;
                    }
                }
                Instr::OP_BRANCH_FALSE_GREATER_EQUAL => {
                    if !state.eval_compare_bool(std::cmp::Ordering::Less, true)? {
                        self.jump(inst.sbx())?;
                    }
                }
                Instr::OP_BRANCH_FALSE_EQUAL => {
                    let val2 = state.pop_val();
                    let val1 = state.pop_val();
                    if val1 != val2 {
                        self.jump(inst.sbx())?;
                    }
                }
                Instr::OP_BRANCH_FALSE_NOT_EQUAL => {
                    let val2 = state.pop_val();
                    let val1 = state.pop_val();
                    if val1 == val2 {
                        self.jump(inst.sbx())?;
                    }
                }

                // `for` loops - control flow is free
                Instr::OP_FOR_LOOP => state.instr_for_loop(self, inst.a(), inst.sbx())?,
                Instr::OP_FOR_PREP => state.instr_for_prep(self, inst.a(), inst.sbx())?,

                // Generic `for` loops - iteration is free
                Instr::OP_TFOR_PREP => state.instr_tfor_prep(inst.a()),
                Instr::OP_TFOR_CALL => {
                    self.record_call_site(state);
                    Self::flush_local_cost(state, &mut local_cost)?;
                    state.instr_tfor_call(inst.a(), inst.b(), inst.c(), &self.runtime.caches)?;
                }
                Instr::OP_TFOR_LOOP => state.instr_tfor_loop(self, inst.a(), inst.sbx())?,

                // Length operator is free
                Instr::OP_LENGTH => {
                    self.record_call_site(state);
                    state.instr_length(&mut local_cost)?;
                }

                // Logical not is free
                Instr::OP_NOT => state.instr_not(),

                // Table reads are free
                Instr::OP_GET_FIELD => {
                    self.record_call_site(state);
                    state.instr_get_field(self, inst.bx(), inst.a(), &mut local_cost)?;
                }
                Instr::OP_GET_TABLE => {
                    self.record_call_site(state);
                    state.instr_get_table(&mut local_cost)?;
                }

                // String concatenation is free. Operand A is the chain
                // length (>= 2). Chained `..` collapses into one OP_CONCAT
                // so a long concat has only one intermediate allocation.
                Instr::OP_CONCAT => state.concat_helper(inst.a() as usize)?,

                // === COSTED OPERATIONS (cost 1) ===

                // Arithmetic costs 1
                Instr::OP_ADD => {
                    add_cost!(state, local_cost, 1);
                    state.eval_float_float(<f64 as ops::Add>::add)?;
                }
                Instr::OP_SUBTRACT => {
                    add_cost!(state, local_cost, 1);
                    state.eval_float_float(<f64 as ops::Sub>::sub)?;
                }
                Instr::OP_MULTIPLY => {
                    add_cost!(state, local_cost, 1);
                    state.eval_float_float(<f64 as ops::Mul>::mul)?;
                }
                Instr::OP_DIVIDE => {
                    add_cost!(state, local_cost, 1);
                    state.eval_float_float(<f64 as ops::Div>::div)?;
                }
                Instr::OP_MOD => {
                    add_cost!(state, local_cost, 1);
                    state.eval_float_float(lua_modulo)?;
                }
                Instr::OP_POW => {
                    add_cost!(state, local_cost, 1);
                    state.eval_float_float(f64::powf)?;
                }

                // Unary negation costs 1
                Instr::OP_NEGATE => {
                    add_cost!(state, local_cost, 1);
                    state.instr_negate()?;
                }

                // Table creation costs 1
                Instr::OP_NEW_TABLE => {
                    add_cost!(state, local_cost, 1);
                    state.new_table()?;
                }
                Instr::OP_NEW_TABLE_PRESIZED => {
                    add_cost!(state, local_cost, 1);
                    state.new_table_with_capacity(inst.a() as usize)?;
                }
                Instr::OP_NEW_TABLE_TEMPLATE => {
                    add_cost!(state, local_cost, 1);
                    state.instr_new_table_template(self, inst.a())?;
                }
                Instr::OP_NEW_TABLE_TRACKED => {
                    add_cost!(state, local_cost, 1);
                    let table_idx = state.stack.len();
                    state.new_table_with_capacity(inst.a() as usize)?;
                    state.table_constructor_bases.push(table_idx);
                }

                // Table writes cost 1
                Instr::OP_INIT_FIELD => {
                    add_cost!(state, local_cost, 1);
                    state.instr_init_field(self, inst.a(), inst.bx())?;
                }
                Instr::OP_INIT_FIELD_PINNED => {
                    add_cost!(state, local_cost, 1);
                    state.instr_init_field_pinned(self, inst.bx(), inst.a())?;
                }
                Instr::OP_INIT_INDEX => {
                    add_cost!(state, local_cost, 1);
                    state.instr_init_index(inst.a())?;
                }
                Instr::OP_SET_FIELD => {
                    self.record_call_site(state);
                    add_cost!(state, local_cost, 1);
                    state.instr_set_field(self, 0, inst.bx(), inst.a(), &mut local_cost)?;
                }
                Instr::OP_SET_FIELD_AT => {
                    self.record_call_site(state);
                    add_cost!(state, local_cost, 1);
                    state.instr_set_field(self, inst.a(), inst.bx(), u8::MAX, &mut local_cost)?;
                }
                Instr::OP_SET_TABLE => {
                    self.record_call_site(state);
                    add_cost!(state, local_cost, 1);
                    state.instr_set_table(inst.a(), &mut local_cost)?;
                }

                // Array initialization: cost per element
                Instr::OP_SET_LIST => {
                    let n = inst.a();
                    let count = state.instr_set_list_count(n)?;
                    add_cost!(state, local_cost, count as u64);
                    state.instr_set_list(n, inst.bx())?;
                }

                // Unknown opcode
                _ => {
                    return Err(Error::without_location(ErrorKind::InternalError(format!(
                        "unknown opcode: {}",
                        inst.opcode()
                    ))));
                }
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
            caches: super::super::compiler::RuntimeCaches::new(&bytecode),
        });
        let mut frame = Frame::new(bytecode, runtime, Arc::from([]), Vec::new(), 0);
        frame.ip = i16::MIN.unsigned_abs() as usize;

        frame
            .jump(i16::MIN)
            .expect("minimum offset should be valid");
        assert_eq!(frame.ip, 0);
    }

    #[test]
    fn jump_rejects_end_of_bytecode() {
        let bytecode = Arc::new(Bytecode {
            code: vec![Instr::ret(RetCount::Fixed(0))],
            ..Bytecode::default()
        });
        let runtime = Arc::new(BytecodeRuntime {
            literals: Box::new([]),
            caches: super::super::compiler::RuntimeCaches::new(&bytecode),
        });
        let mut frame = Frame::new(bytecode, runtime, Arc::from([]), Vec::new(), 0);

        let error = frame.jump(1).expect_err("jump to end must be rejected");
        assert!(matches!(
            error.kind,
            ErrorKind::InvalidJump { ip: 0, offset: 1 }
        ));
    }
}
