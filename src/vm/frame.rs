use std::ops;
use std::rc::Rc;

use super::super::compiler::UpvalueDesc;
use super::super::error::{Error, ErrorKind, TypeError};
use super::object::{Upvalue, UpvalueRef};
use super::Chunk;
use super::Instr;
use super::LuaType;
use super::Result;
use super::State;
use super::Val;
use crate::instr::{ArgCount, RetCount};

/// A `Frame` represents a single stack-frame of a Lua function.
pub(super) struct Frame {
    /// The chunk being executed (shared via Rc to avoid cloning)
    chunk: Rc<Chunk>,
    /// The index of the next (not current) instruction
    ip: usize,
    /// Offset into `State.string_literals` where this chunk's literals are
    /// stored.
    string_literal_start: usize,
    /// The upvalues captured by this closure.
    upvalues: Vec<UpvalueRef>,
    /// The varargs passed to this function (if it's a vararg function).
    varargs: Vec<Val>,
    /// The stack bottom when this frame was created (used for closing upvalues).
    pub(super) stack_bottom: usize,
}

impl Frame {
    /// Create a new Frame.
    #[must_use]
    pub(super) fn new(
        chunk: Rc<Chunk>,
        upvalues: Vec<UpvalueRef>,
        varargs: Vec<Val>,
        string_literal_start: usize,
        stack_bottom: usize,
    ) -> Self {
        let ip = 0;
        Self {
            chunk,
            ip,
            string_literal_start,
            upvalues,
            varargs,
            stack_bottom,
        }
    }

    /// Jump forward/back by `offset` instructions.
    fn jump(&mut self, offset: i16) -> Result<()> {
        let new_ip = if offset >= 0 {
            self.ip.checked_add(offset as usize)
        } else {
            self.ip.checked_sub((-offset) as usize)
        };
        match new_ip {
            Some(ip) if ip <= self.chunk.code.len() => {
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
        let i = self.chunk.code[self.ip];
        self.ip += 1;
        i
    }

    #[must_use]
    fn get_nested_chunk(&mut self, i: u8) -> Chunk {
        self.chunk.nested[i as usize].clone()
    }

    #[must_use]
    fn get_number_constant(&self, i: u8) -> f64 {
        self.chunk.number_literals[i as usize]
    }

    /// How often to flush accumulated cost to the state.
    /// Higher values reduce overhead but may overshoot budget more.
    const COST_CHECK_INTERVAL: u64 = 64;

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
                $local += $cost;
                if $local >= Self::COST_CHECK_INTERVAL {
                    $state.consume_cost($local)?;
                    $local = 0;
                }
            }};
        }

        loop {
            let inst = self.get_instr();
            #[cfg(feature = "debug_vm")]
            println!("{:?}", inst);
            match inst.opcode() {
                // === FREE OPERATIONS (cost 0) ===

                // General control flow
                Instr::OP_POP => {
                    state.pop_val();
                }
                Instr::OP_DUP => {
                    let val = state.stack.last().unwrap().clone();
                    state.stack.push(val);
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
                Instr::OP_GET_LOCAL => state.instr_get_local(inst.a()),
                Instr::OP_SET_LOCAL => state.instr_set_local(inst.a()),

                // Upvalues
                Instr::OP_GET_UPVALUE => state.instr_get_upvalue(self, inst.a()),
                Instr::OP_SET_UPVALUE => state.instr_set_upvalue(self, inst.a()),

                // Globals
                Instr::OP_GET_GLOBAL => state.instr_get_global(self, inst.a()),
                Instr::OP_SET_GLOBAL => state.instr_set_global(self, inst.a())?,

                // Functions (calls and returns are free)
                Instr::OP_CLOSURE => state.instr_closure(self, inst.a()),
                Instr::OP_CALL => state.call(ArgCount::from_u8(inst.a()), RetCount::from_u8(inst.b()))?,
                Instr::OP_MARK_CALL_BASE => {
                    state.vararg_call_bases.push(state.stack.len());
                }
                Instr::OP_CLOSE_UPVALUES => {
                    // Close upvalues for locals at or above the given slot
                    // Used at end of loop iterations to capture per-iteration locals
                    let stack_level = state.stack_bottom + inst.a() as usize;
                    state.close_upvalues(stack_level);
                }
                Instr::OP_RETURN => {
                    // Flush any remaining accumulated cost before returning
                    if local_cost > 0 {
                        state.consume_cost(local_cost)?;
                    }
                    return Ok(RetCount::from_u8(inst.a()));
                }
                Instr::OP_VARARG => {
                    let n = inst.a();
                    if n == u8::MAX {
                        // Push all varargs
                        for val in &self.varargs {
                            state.stack.push(val.clone());
                        }
                    } else {
                        // Push exactly n values, padding with nil if needed
                        let n = n as usize;
                        for i in 0..n {
                            if i < self.varargs.len() {
                                state.stack.push(self.varargs[i].clone());
                            } else {
                                state.push_nil();
                            }
                        }
                    }
                }

                // Literals (free)
                Instr::OP_PUSH_NIL => state.push_nil(),
                Instr::OP_PUSH_BOOL => state.push_boolean(inst.a() != 0),
                Instr::OP_PUSH_NUM => {
                    let n = self.get_number_constant(inst.a());
                    state.push_number(n);
                }
                Instr::OP_PUSH_STRING => {
                    let val = state.get_string_constant(self, inst.a());
                    state.stack.push(val);
                }

                // Equality (comparisons are free)
                Instr::OP_EQUAL => {
                    let val2 = state.pop_val();
                    let val1 = state.pop_val();
                    state.push_boolean(val1 == val2);
                }
                Instr::OP_NOT_EQUAL => {
                    let val2 = state.pop_val();
                    let val1 = state.pop_val();
                    state.push_boolean(val1 != val2);
                }

                // Orderings (comparisons are free)
                // Supports both number and string comparisons
                Instr::OP_LESS => state.eval_compare(std::cmp::Ordering::Less, false)?,
                Instr::OP_GREATER => state.eval_compare(std::cmp::Ordering::Greater, false)?,
                Instr::OP_LESS_EQUAL => state.eval_compare(std::cmp::Ordering::Greater, true)?,  // <= is !>
                Instr::OP_GREATER_EQUAL => state.eval_compare(std::cmp::Ordering::Less, true)?,  // >= is !<

                // `for` loops - control flow is free
                Instr::OP_FOR_LOOP => state.instr_for_loop(self, inst.a(), inst.sbx())?,
                Instr::OP_FOR_PREP => state.instr_for_prep(self, inst.a(), inst.sbx())?,

                // Generic `for` loops - iteration is free
                Instr::OP_TFOR_PREP => state.instr_tfor_prep(inst.a()),
                Instr::OP_TFOR_CALL => state.instr_tfor_call(inst.a(), inst.b())?,
                Instr::OP_TFOR_LOOP => state.instr_tfor_loop(self, inst.a(), inst.sbx())?,

                // Length operator is free
                Instr::OP_LENGTH => state.instr_length()?,

                // Logical not is free
                Instr::OP_NOT => state.instr_not(),

                // Table reads are free
                Instr::OP_GET_FIELD => state.instr_get_field(self, inst.a())?,
                Instr::OP_GET_TABLE => state.instr_get_table()?,

                // String concatenation is free
                Instr::OP_CONCAT => state.concat_helper(2)?,

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
                    // Lua uses floored modulo: a % b = a - floor(a/b) * b
                    // This differs from Rust's remainder when signs differ
                    state.eval_float_float(|a, b| a - (a / b).floor() * b)?;
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
                    state.new_table();
                }

                // Table writes cost 1
                Instr::OP_INIT_FIELD => {
                    add_cost!(state, local_cost, 1);
                    state.instr_init_field(self, inst.a(), inst.b())?;
                }
                Instr::OP_INIT_INDEX => {
                    add_cost!(state, local_cost, 1);
                    state.instr_init_index(inst.a())?;
                }
                Instr::OP_SET_FIELD => {
                    add_cost!(state, local_cost, 1);
                    state.instr_set_field(self, inst.a(), inst.b())?;
                }
                Instr::OP_SET_TABLE => {
                    add_cost!(state, local_cost, 1);
                    state.instr_set_table(inst.a())?;
                }

                // Array initialization: cost per element
                Instr::OP_SET_LIST => {
                    let n = inst.a();
                    let count = if n == 0 {
                        // SetList(0) means "use all values above table"
                        // We'll charge based on actual count after operation
                        // For now, just charge 1 for the operation
                        1
                    } else {
                        n as u64
                    };
                    add_cost!(state, local_cost, count);
                    state.instr_set_list(n)?;
                }

                // Unknown opcode
                _ => {
                    return Err(Error::without_location(ErrorKind::InternalError(
                        format!("unknown opcode: {}", inst.opcode())
                    )));
                }
            }
        }
    }
}

// Instruction-specific methods
impl State {
    /// Pop a value. If its truthiness matches `cond`, jump with `offset`.
    /// If `keep_cond`, then push the value back after jumping.
    fn instr_branch(
        &mut self,
        frame: &mut Frame,
        cond: bool,
        offset: i16,
        keep_cond: bool,
    ) -> Result<()> {
        let val = self.pop_val();
        let truthy = val.truthy();
        if cond == truthy {
            frame.jump(offset)?;
        }
        if keep_cond {
            self.stack.push(val);
        }
        Ok(())
    }

    fn instr_closure(&mut self, frame: &mut Frame, i: u8) {
        let chunk = frame.get_nested_chunk(i);
        // Capture upvalues based on the chunk's upvalue descriptors
        let mut captured_upvalues = Vec::with_capacity(chunk.upvalues.len());
        for desc in &chunk.upvalues {
            let uv_ref = match desc {
                UpvalueDesc::Local(idx) => {
                    // Capture a local variable from the current frame's stack
                    let stack_idx = frame.stack_bottom + *idx as usize;
                    self.find_or_create_upvalue(stack_idx)
                }
                UpvalueDesc::Upvalue(idx) => {
                    // Share an upvalue from the current frame's upvalues
                    frame.upvalues[*idx as usize].clone()
                }
            };
            captured_upvalues.push(uv_ref);
        }
        self.push_closure(chunk, captured_upvalues);
    }

    fn instr_for_prep(&mut self, frame: &mut Frame, local: u8, body_len: i16) -> Result<()> {
        // These slots should only be assigned to during this function.
        let step = self.pop_val().as_num().unwrap();
        let end = self.pop_val().as_num().unwrap();
        let start = self.pop_val().as_num().unwrap();
        if check_numeric_for_condition(start, end, step) {
            let mut local_slot = local as usize + self.stack_bottom;
            for &n in &[start, end, step, start] {
                self.stack[local_slot] = Val::Num(n);
                local_slot += 1;
            }
        } else {
            frame.jump(body_len)?;
        }
        Ok(())
    }

    fn instr_for_loop(&mut self, frame: &mut Frame, local_slot: u8, offset: i16) -> Result<()> {
        let slot = local_slot as usize + self.stack_bottom;
        let mut var = self.stack[slot].as_num().unwrap();
        let limit = self.stack[slot + 1].as_num().unwrap();
        let step = self.stack[slot + 2].as_num().unwrap();
        var += step;
        if check_numeric_for_condition(var, limit, step) {
            self.stack[slot] = Val::Num(var);
            self.stack[slot + 3] = Val::Num(var);
            frame.jump(offset)?;
        }
        Ok(())
    }

    /// TForPrep: Pop 3 values (iterator, state, control) and store in locals.
    fn instr_tfor_prep(&mut self, local_slot: u8) {
        let base = local_slot as usize + self.stack_bottom;
        // Pop in reverse order: control, state, iterator
        let control = self.pop_val();
        let state = self.pop_val();
        let iterator = self.pop_val();
        // Store in order: iterator, state, control
        self.stack[base] = iterator;
        self.stack[base + 1] = state;
        self.stack[base + 2] = control;
    }

    /// TForCall: Call iterator(state, control), store results in loop variable slots.
    fn instr_tfor_call(&mut self, local_slot: u8, num_vars: u8) -> Result<()> {
        let base = local_slot as usize + self.stack_bottom;
        // Push iterator function, state, and control onto stack for call
        let iterator = self.stack[base].clone();
        let state = self.stack[base + 1].clone();
        let control = self.stack[base + 2].clone();

        self.stack.push(iterator);
        self.stack.push(state);
        self.stack.push(control);

        // Call with 2 args (state, control), expecting num_vars returns
        self.call(ArgCount::Fixed(2), RetCount::Fixed(num_vars))?;

        // Move results from stack to loop variable slots (base + 3, base + 4, ...)
        let results_start = self.stack.len() - num_vars as usize;
        for i in 0..num_vars as usize {
            self.stack[base + 3 + i] = self.stack[results_start + i].clone();
        }
        // Pop the results from stack
        self.stack.truncate(results_start);

        Ok(())
    }

    /// TForLoop: If first loop variable is nil, jump. Otherwise update control var.
    fn instr_tfor_loop(
        &mut self,
        frame: &mut Frame,
        local_slot: u8,
        offset: i16,
    ) -> Result<()> {
        let base = local_slot as usize + self.stack_bottom;
        let first_var = &self.stack[base + 3];

        if matches!(first_var, Val::Nil) {
            // Exit loop
            frame.jump(offset)?;
        } else {
            // Update control variable with first loop variable
            self.stack[base + 2] = self.stack[base + 3].clone();
        }
        Ok(())
    }

    fn instr_get_field(&mut self, frame: &mut Frame, field_id: u8) -> Result<()> {
        // Pop value, handle both tables and strings
        let val = self.pop_val();
        let key = self.get_string_constant(frame, field_id);

        if val.as_table_ref().is_some() {
            // Table: use get_table_with_key for metamethod support
            self.stack.push(val);
            let table_idx = self.stack.len() - 1;
            self.get_table_with_key(table_idx, key.clone())?;
            // Stack now: [... table, result]
            let result = self.pop_val();
            self.pop_val(); // Remove table

            // If result is nil, fall back to the 'table' global library
            // This enables tbl:insert(), tbl:concat() etc.
            if matches!(result, Val::Nil) {
                self.get_global("table");
                let table_lib_idx = self.stack.len() - 1;
                self.get_table_with_key(table_lib_idx, key)?;
                let lib_result = self.pop_val();
                self.pop_val(); // Remove table_lib
                self.stack.push(lib_result);
            } else {
                self.stack.push(result);
            }
            Ok(())
        } else if val.as_string().is_some() {
            // String: look up method in the 'string' global table
            self.get_global("string");
            let string_lib_idx = self.stack.len() - 1;
            self.get_table_with_key(string_lib_idx, key)?;
            // Stack now: [... string_lib, result]
            let result = self.pop_val();
            self.pop_val(); // Remove string_lib
            self.stack.push(result);
            Ok(())
        } else {
            Err(self.type_error(TypeError::TableIndex(val.typ())))
        }
    }

    fn instr_get_global(&mut self, frame: &Frame, string_num: u8) {
        let s = &frame.chunk.string_literals[string_num as usize];
        self.get_global(s);
    }

    #[inline(always)]
    fn instr_get_local(&mut self, local_num: u8) {
        let i = local_num as usize + self.stack_bottom;
        let val = self.stack[i].clone();
        self.stack.push(val);
    }

    fn instr_get_upvalue(&mut self, frame: &Frame, upvalue_num: u8) {
        let uv_ref = frame.upvalues[upvalue_num as usize];
        let val = match self.upvalue_pool.get(uv_ref) {
            Upvalue::Open(stack_idx) => self.stack[*stack_idx].clone(),
            Upvalue::Closed(v) => v.clone(),
        };
        self.stack.push(val);
    }

    fn instr_set_upvalue(&mut self, frame: &Frame, upvalue_num: u8) {
        let val = self.pop_val();
        let uv_ref = frame.upvalues[upvalue_num as usize];
        match self.upvalue_pool.get(uv_ref).clone() {
            Upvalue::Open(stack_idx) => {
                self.stack[stack_idx] = val;
            }
            Upvalue::Closed(_) => {
                *self.upvalue_pool.get_mut(uv_ref) = Upvalue::Closed(val);
            }
        }
    }

    fn instr_get_table(&mut self) -> Result<()> {
        let key = self.pop_val();
        // Table is now on top of the stack
        let tbl_val = self.stack.last().unwrap();
        if tbl_val.as_table_ref().is_none() {
            let typ = tbl_val.typ();
            self.pop_val();
            return Err(self.type_error(TypeError::TableIndex(typ)));
        }
        let table_idx = self.stack.len() - 1;
        self.get_table_with_key(table_idx, key)?;
        // Stack now: [... table, result]
        let result = self.pop_val();
        self.pop_val(); // Remove table
        self.stack.push(result);
        Ok(())
    }

    fn instr_init_field(&mut self, frame: &Frame, negative_offset: u8, key_id: u8) -> Result<()> {
        let val = self.pop_val();
        let positive_offset = self.stack.len() - negative_offset as usize - 1;
        let mut tbl_value = self.stack[positive_offset].clone();
        if let Some(tbl) = tbl_value.as_table() {
            let key = self.get_string_constant(frame, key_id);
            tbl.insert(key, val)?;
            Ok(())
        } else {
            Err(self.error(ErrorKind::InternalError(format!(
                "InitField: expected table, got {}",
                tbl_value.typ()
            ))))
        }
    }

    fn instr_init_index(&mut self, negative_offset: u8) -> Result<()> {
        let val = self.pop_val();
        let key = self.pop_val();
        let positive_offset = self.stack.len() - negative_offset as usize - 1;
        let tbl_typ = self.stack[positive_offset].typ();
        let tbl = &mut self.stack[positive_offset];
        match tbl.as_table() {
            Some(tbl) => {
                tbl.insert(key, val)?;
                Ok(())
            }
            None => Err(self.error(ErrorKind::InternalError(format!(
                "InitIndex: expected table, got {}",
                tbl_typ
            )))),
        }
    }

    fn instr_length(&mut self) -> Result<()> {
        let val = self.pop_val();
        match val.typ() {
            LuaType::String => {
                let s = val.as_string().unwrap();
                let len = s.len();
                self.stack.push(Val::Num(len as f64));
                Ok(())
            }
            LuaType::Table => {
                let tbl = val.as_table_ref().unwrap();
                // Check for __len metamethod
                if let Some(mt_ptr) = tbl.get_metatable() {
                    let len_key = self.alloc_string("__len".to_string());
                    if let Some(mt) = Val::Obj(mt_ptr).as_table() {
                        let len_handler = mt.get(&len_key);
                        if !matches!(len_handler, Val::Nil) {
                            // Call __len(table)
                            self.stack.push(len_handler);
                            self.stack.push(val);
                            self.call(ArgCount::Fixed(1), RetCount::Fixed(1))?;
                            return Ok(());
                        }
                    }
                }
                // No __len, use default array_len
                let len = tbl.array_len();
                self.stack.push(Val::Num(len as f64));
                Ok(())
            }
            typ => Err(self.type_error(TypeError::Length(typ))),
        }
    }

    fn instr_negate(&mut self) -> Result<()> {
        let n = self.pop_num()?;
        self.stack.push(Val::Num(-n));
        Ok(())
    }

    fn instr_not(&mut self) {
        let b = self.pop_val().truthy();
        self.stack.push(Val::Bool(!b));
    }

    fn instr_set_field(&mut self, frame: &Frame, stack_offset: u8, field_id: u8) -> Result<()> {
        let val = self.pop_val();
        let idx = self.stack.len() - stack_offset as usize - 1;
        let tbl_val = &self.stack[idx];
        if tbl_val.as_table_ref().is_none() {
            let typ = tbl_val.typ();
            return Err(self.type_error(TypeError::TableIndex(typ)));
        }
        let key = self.get_string_constant(frame, field_id);
        self.set_table_with_key(idx, key, val)?;
        self.stack.remove(idx);
        Ok(())
    }

    fn instr_set_global(&mut self, frame: &Frame, string_num: u8) -> Result<()> {
        let s = self.get_string_constant(frame, string_num);
        let val = self.pop_val();
        if let Some(s) = s.as_string() {
            self.globals.insert(s.into(), val);
            Ok(())
        } else {
            Err(self.error(ErrorKind::InternalError(format!(
                "SetGlobal: expected string constant, got {}",
                s.typ()
            ))))
        }
    }

    fn instr_set_list(&mut self, count: u8) -> Result<()> {
        // Find the table on the stack (it's below the values)
        // count=0 means "use all values above the table"
        let values = if count == 0 {
            // Find the table - it's the first table value scanning from the bottom of current frame
            let mut table_idx = None;
            for i in self.stack_bottom..self.stack.len() {
                if self.stack[i].as_table_ref().is_some() {
                    table_idx = Some(i);
                    break;
                }
            }
            let table_idx = match table_idx {
                Some(idx) => idx,
                None => {
                    return Err(self.error(ErrorKind::InternalError(
                        "SetList(0): no table found on stack".to_string(),
                    )));
                }
            };
            self.stack.split_off(table_idx + 1)
        } else {
            self.stack.split_off(self.stack.len() - count as usize)
        };
        let mut tbl_value = self.pop_val();
        if let Some(tbl) = tbl_value.as_table() {
            let counter = 1..;
            for (i, val) in counter.zip(values) {
                let key = Val::Num(i as f64);
                tbl.insert(key, val)?;
            }
            self.stack.push(tbl_value);
            Ok(())
        } else {
            Err(self.error(ErrorKind::InternalError(format!(
                "SetList: expected table, got {}",
                tbl_value.typ()
            ))))
        }
    }

    #[inline(always)]
    fn instr_set_local(&mut self, local_num: u8) {
        let val = self.pop_val();
        let i = local_num as usize + self.stack_bottom;
        self.stack[i] = val;
    }

    fn instr_set_table(&mut self, offset: u8) -> Result<()> {
        let val = self.pop_val();
        let index = self.stack.len() - offset as usize - 2;
        let key = self.stack.remove(index + 1); // Remove the key first (it's after the table)
        let tbl_val = &self.stack[index];
        if tbl_val.as_table_ref().is_none() {
            let typ = tbl_val.typ();
            return Err(self.type_error(TypeError::TableIndex(typ)));
        }
        self.set_table_with_key(index, key, val)?;
        self.stack.remove(index); // Remove the table
        Ok(())
    }

    // Helper methods

    /// Compare two values (numbers or strings).
    /// `target` is the ordering we're checking for.
    /// `negate` inverts the result (for <= and >=).
    fn eval_compare(&mut self, target: std::cmp::Ordering, negate: bool) -> Result<()> {
        let v2 = self.pop_val();
        let v1 = self.pop_val();

        let result = match (&v1, &v2) {
            (Val::Num(n1), Val::Num(n2)) => {
                let cmp = n1.partial_cmp(n2).unwrap_or(std::cmp::Ordering::Equal);
                cmp == target
            }
            (Val::Str(s1), Val::Str(s2)) => {
                let cmp = s1.as_str().cmp(s2.as_str());
                cmp == target
            }
            _ => {
                // Type mismatch - error
                return Err(self.error(ErrorKind::TypeError(TypeError::Comparison(v1.typ(), v2.typ()))));
            }
        };

        self.stack.push(Val::Bool(if negate { !result } else { result }));
        Ok(())
    }

    #[inline(always)]
    fn eval_float_float(&mut self, f: impl Fn(f64, f64) -> f64) -> Result<()> {
        let n2 = self.pop_num()?;
        let n1 = self.pop_num()?;
        self.stack.push(Val::Num(f(n1, n2)));
        Ok(())
    }

    fn get_string_constant(&self, frame: &Frame, i: u8) -> Val {
        // self.string_literals[i as usize].clone()
        let index = frame.string_literal_start + i as usize;
        self.string_literals[index].clone()
    }

    fn pop_num(&mut self) -> Result<f64> {
        let val = self.pop_val();
        val.as_num()
            .ok_or_else(|| self.type_error(TypeError::Arithmetic(val.typ())))
    }
}

fn check_numeric_for_condition(var: f64, limit: f64, step: f64) -> bool {
    // Step of zero would cause infinite loop - skip the loop entirely
    if step == 0.0 {
        false
    } else if step > 0.0 {
        var <= limit
    } else {
        // step < 0.0
        var >= limit
    }
}
