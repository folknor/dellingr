use std::ops;
use std::rc::Rc;
use std::str;

use super::super::compiler::UpvalueDesc;
use super::super::compiler::{
    FieldLookupCacheEntry, FieldLookupCacheSlot, GlobalLookupCacheEntry, GlobalLookupCacheSlot,
};
use super::super::error::{Error, ErrorKind, StackFrame, TypeError};
use super::Chunk;
use super::Instr;
use super::Result;
use super::State;
use super::Val;
use super::lua_val::RustFunc;
use super::object::{ObjectPtr, Upvalue, UpvalueRef};
use crate::instr::{ArgCount, RetCount};
use crate::lua_std::{base_ipairs_iter, base_next};

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

    /// Get the chunk being executed.
    #[allow(dead_code)]
    pub(super) fn chunk(&self) -> &Rc<Chunk> {
        &self.chunk
    }

    pub(super) fn string_literal_start(&self) -> usize {
        self.string_literal_start
    }

    /// Get the current line number (1-indexed), or 0 if unknown.
    pub(super) fn current_line(&self) -> u32 {
        // ip points to the NEXT instruction, so use ip-1 for current
        let idx = self.ip.saturating_sub(1);
        self.chunk.line_info.get(idx).copied().unwrap_or(0)
    }

    /// Create a StackFrame for error reporting.
    pub(super) fn to_stack_frame(&self) -> StackFrame {
        StackFrame {
            function_name: self.chunk.name.clone(),
            source: self.chunk.source.clone(),
            line: self.current_line(),
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
            println!("{inst:?}");
            match inst.opcode() {
                // === FREE OPERATIONS (cost 0) ===

                // General control flow
                Instr::OP_POP => {
                    state.pop_val();
                }
                Instr::OP_DUP => {
                    let val = *state
                        .stack
                        .last()
                        .expect("Dup instruction requires a stack value");
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
                Instr::OP_GET_GLOBAL => state.instr_get_global(self, inst.a(), inst.bx())?,
                Instr::OP_SET_GLOBAL => state.instr_set_global(self, inst.a())?,

                // Builtins (fast path for well-known globals)
                Instr::OP_GET_BUILTIN => state.instr_get_builtin(inst.a()),
                Instr::OP_SET_BUILTIN => state.instr_set_builtin(inst.a()),

                // Functions (calls and returns are free)
                Instr::OP_CLOSURE => state.instr_closure(self, inst.a()),
                Instr::OP_CALL => {
                    // Update current CallInfo with the call site (ip-1 since we already advanced)
                    if let Some(call_info) = state.call_stack.last_mut() {
                        call_info.ip = self.ip;
                    }
                    state.call(ArgCount::from_u8(inst.a()), RetCount::from_u8(inst.b()))?;
                }
                Instr::OP_MARK_CALL_BASE => {
                    let adjustment = inst.a() as usize;
                    state.vararg_call_bases.push(state.stack.len() - adjustment);
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
                            state.stack.push(*val);
                        }
                    } else {
                        // Push exactly n values, padding with nil if needed
                        let n = n as usize;
                        for i in 0..n {
                            if i < self.varargs.len() {
                                state.stack.push(self.varargs[i]);
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
                Instr::OP_LESS_EQUAL => state.eval_compare(std::cmp::Ordering::Greater, true)?, // <= is !>
                Instr::OP_GREATER_EQUAL => state.eval_compare(std::cmp::Ordering::Less, true)?, // >= is !<

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
                Instr::OP_GET_FIELD => state.instr_get_field(self, inst.a(), inst.bx())?,
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
                    let count = state.instr_set_list(n)?;
                    add_cost!(state, local_cost, count as u64);
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

// Instruction-specific methods
impl State {
    /// Pop a value. If its truthiness matches `cond`, jump with `offset`.
    /// If `keep_cond`, then push the value back after jumping.
    #[hotpath::measure]
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

    #[hotpath::measure]
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
                    frame.upvalues[*idx as usize]
                }
            };
            captured_upvalues.push(uv_ref);
        }
        self.push_closure(chunk, captured_upvalues);
    }

    #[hotpath::measure]
    fn instr_for_prep(&mut self, frame: &mut Frame, local: u8, body_len: i16) -> Result<()> {
        let step_val = self.pop_val();
        let end_val = self.pop_val();
        let start_val = self.pop_val();
        let step = step_val
            .as_num()
            .ok_or_else(|| self.type_error(TypeError::Arithmetic(step_val.typ_simple())))?;
        let end = end_val
            .as_num()
            .ok_or_else(|| self.type_error(TypeError::Arithmetic(end_val.typ_simple())))?;
        let start = start_val
            .as_num()
            .ok_or_else(|| self.type_error(TypeError::Arithmetic(start_val.typ_simple())))?;
        if check_numeric_for_condition(start, end, step) {
            for (local_slot, n) in
                (local as usize + self.stack_bottom..).zip([start, end, step, start])
            {
                self.stack[local_slot] = Val::Num(n);
            }
        } else {
            frame.jump(body_len)?;
        }
        Ok(())
    }

    #[hotpath::measure]
    fn instr_for_loop(&mut self, frame: &mut Frame, local_slot: u8, offset: i16) -> Result<()> {
        let slot = local_slot as usize + self.stack_bottom;
        let mut var = self.stack[slot]
            .as_num()
            .ok_or_else(|| self.type_error(TypeError::Arithmetic(self.stack[slot].typ_simple())))?;
        let limit = self.stack[slot + 1].as_num().ok_or_else(|| {
            self.type_error(TypeError::Arithmetic(self.stack[slot + 1].typ_simple()))
        })?;
        let step = self.stack[slot + 2].as_num().ok_or_else(|| {
            self.type_error(TypeError::Arithmetic(self.stack[slot + 2].typ_simple()))
        })?;
        var += step;
        if check_numeric_for_condition(var, limit, step) {
            self.stack[slot] = Val::Num(var);
            self.stack[slot + 3] = Val::Num(var);
            frame.jump(offset)?;
        }
        Ok(())
    }

    /// TForPrep: Pop 3 values (iterator, state, control) and store in locals.
    #[hotpath::measure]
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
    #[hotpath::measure]
    fn instr_tfor_call(&mut self, local_slot: u8, num_vars: u8) -> Result<()> {
        let base = local_slot as usize + self.stack_bottom;
        // Push iterator function, state, and control onto stack for call
        let iterator = self.stack[base];
        let state = self.stack[base + 1];
        let control = self.stack[base + 2];

        if let Val::RustFn(f) = iterator {
            let base_next_fn: RustFunc = base_next;
            let base_ipairs_iter_fn: RustFunc = base_ipairs_iter;
            if std::ptr::fn_addr_eq(f, base_next_fn)
                && self.instr_tfor_call_next(base, state, control, num_vars)
            {
                return Ok(());
            }
            if std::ptr::fn_addr_eq(f, base_ipairs_iter_fn)
                && self.instr_tfor_call_ipairs(base, state, control, num_vars)
            {
                return Ok(());
            }
            return self.instr_tfor_call_rust_fn(f, base, state, control, num_vars);
        }

        self.stack.push(iterator);
        self.stack.push(state);
        self.stack.push(control);

        // Call with 2 args (state, control), expecting num_vars returns
        self.call(ArgCount::Fixed(2), RetCount::Fixed(num_vars))?;

        // Move results from stack to loop variable slots (base + 3, base + 4, ...)
        let results_start = self.stack.len() - num_vars as usize;
        for i in 0..num_vars as usize {
            self.stack[base + 3 + i] = self.stack[results_start + i];
        }
        // Pop the results from stack
        self.stack.truncate(results_start);

        Ok(())
    }

    #[inline(always)]
    fn write_tfor_results(&mut self, base: usize, num_vars: u8, first: Val, second: Option<Val>) {
        for i in 0..num_vars as usize {
            self.stack[base + 3 + i] = match i {
                0 => first,
                1 => second.unwrap_or(Val::Nil),
                _ => Val::Nil,
            };
        }
    }

    fn instr_tfor_call_next(
        &mut self,
        base: usize,
        state: Val,
        control: Val,
        num_vars: u8,
    ) -> bool {
        let Some(tbl) = state
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
        else {
            return false;
        };

        let (next_key, next_val) = tbl.next(&control);
        if matches!(next_key, Val::Nil) {
            self.write_tfor_results(base, num_vars, Val::Nil, None);
        } else {
            self.write_tfor_results(base, num_vars, next_key, Some(next_val));
        }
        true
    }

    fn instr_tfor_call_ipairs(
        &mut self,
        base: usize,
        state: Val,
        control: Val,
        num_vars: u8,
    ) -> bool {
        let Some(old_index) = control.as_num() else {
            return false;
        };
        let Some(tbl) = state
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
        else {
            return false;
        };

        let new_index = old_index + 1.0;
        let key = Val::Num(new_index);
        let val = tbl.get(&key);
        if matches!(val, Val::Nil) && tbl.get_metatable().is_some() {
            return false;
        }

        if matches!(val, Val::Nil) {
            self.write_tfor_results(base, num_vars, Val::Nil, None);
        } else {
            self.write_tfor_results(base, num_vars, key, Some(val));
        }
        true
    }

    fn instr_tfor_call_rust_fn(
        &mut self,
        f: RustFunc,
        base: usize,
        state: Val,
        control: Val,
        num_vars: u8,
    ) -> Result<()> {
        let old_stack_bottom = self.stack_bottom;
        let call_base = self.stack.len();

        self.stack.push(state);
        self.stack.push(control);
        self.stack_bottom = call_base;

        let result = f(self);
        let num_ret_reported = match result {
            Ok(n) => n,
            Err(e) => {
                self.stack.truncate(call_base);
                self.stack_bottom = old_stack_bottom;
                return Err(e);
            }
        };

        let num_ret_actual = self.get_top() as u8;
        match num_ret_reported.cmp(&num_ret_actual) {
            std::cmp::Ordering::Greater => {
                for _ in num_ret_actual..num_ret_reported {
                    self.push_nil();
                }
            }
            std::cmp::Ordering::Less => {
                let slc = &mut self.stack[self.stack_bottom..];
                slc.rotate_right(num_ret_reported as usize);
                let new_len =
                    self.stack.len() - num_ret_actual as usize + num_ret_reported as usize;
                self.stack.truncate(new_len);
            }
            std::cmp::Ordering::Equal => (),
        }
        self.stack_bottom = old_stack_bottom;

        self.balance_stack(num_vars as usize, num_ret_reported as usize);
        let results_start = self.stack.len() - num_vars as usize;
        for i in 0..num_vars as usize {
            self.stack[base + 3 + i] = self.stack[results_start + i];
        }
        self.stack.truncate(results_start);

        Ok(())
    }

    /// TForLoop: If first loop variable is nil, jump. Otherwise update control var.
    #[hotpath::measure]
    fn instr_tfor_loop(&mut self, frame: &mut Frame, local_slot: u8, offset: i16) -> Result<()> {
        let base = local_slot as usize + self.stack_bottom;
        let first_var = &self.stack[base + 3];

        if matches!(first_var, Val::Nil) {
            // Exit loop
            frame.jump(offset)?;
        } else {
            // Update control variable with first loop variable
            self.stack[base + 2] = self.stack[base + 3];
        }
        Ok(())
    }

    #[hotpath::measure]
    fn instr_get_field(&mut self, frame: &mut Frame, field_id: u8, cache_idx: u16) -> Result<()> {
        // Pop value, handle both tables and strings
        let val = self.pop_val();
        let key = self.get_string_constant(frame, field_id);

        if let Some(ptr) = val.as_object_ptr()
            && let Some((direct, has_metatable)) = self.get_table_field_direct(
                ptr,
                key,
                frame.chunk.field_lookup_cache.get(cache_idx as usize),
            )
        {
            if let Some(result) = direct {
                self.stack.push(result);
                return Ok(());
            }

            if !has_metatable {
                return self.push_table_library_field(key);
            }

            self.stack.push(val);
            let table_idx = self.stack.len() - 1;
            self.get_table_with_key(table_idx, key)?;
            let result = self.pop_val();
            self.pop_val();

            if matches!(result, Val::Nil) {
                self.push_table_library_field(key)
            } else {
                self.stack.push(result);
                Ok(())
            }
        } else if val.as_string_ptr().is_some() {
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
            Err(self.type_error(TypeError::TableIndex(val.typ_simple())))
        }
    }

    #[inline(always)]
    fn get_table_field_direct(
        &self,
        ptr: ObjectPtr,
        key: Val,
        cache: Option<&FieldLookupCacheSlot>,
    ) -> Option<(Option<Val>, bool)> {
        if let Some(val) = cache.and_then(|cache| self.get_cached_field(ptr, key, cache)) {
            return Some((Some(val), false));
        }

        let tbl = self.heap.as_table_ref(ptr)?;
        if let Some((index, val)) = tbl.get_with_index(&key) {
            if let Some(cache) = cache {
                cache.set(FieldLookupCacheEntry {
                    table: ptr,
                    table_version: tbl.version(),
                    index,
                });
            }
            return Some((Some(val), tbl.get_metatable().is_some()));
        }

        Some((None, tbl.get_metatable().is_some()))
    }

    #[inline(always)]
    fn get_cached_field(
        &self,
        ptr: ObjectPtr,
        key: Val,
        cache: &FieldLookupCacheSlot,
    ) -> Option<Val> {
        let entry = cache.get()?;
        if entry.table != ptr {
            return None;
        }
        let tbl = self.heap.as_table_ref(ptr)?;
        let table_version = tbl.version();
        if entry.table_version == table_version {
            return tbl.get_index(entry.index).map(|(_, val)| val);
        }
        let (cached_key, cached_val) = tbl.get_index(entry.index)?;
        if cached_key == key {
            cache.set(FieldLookupCacheEntry {
                table: ptr,
                table_version,
                index: entry.index,
            });
            Some(cached_val)
        } else {
            None
        }
    }

    #[inline(always)]
    fn push_table_library_field(&mut self, key: Val) -> Result<()> {
        self.get_global("table");
        let table_lib_idx = self.stack.len() - 1;
        self.get_table_with_key(table_lib_idx, key)?;
        let result = self.pop_val();
        self.pop_val();
        self.stack.push(result);
        Ok(())
    }

    fn instr_get_global(&mut self, frame: &Frame, string_num: u8, cache_idx: u16) -> Result<()> {
        let s = &frame.chunk.string_literals[string_num as usize];
        let cache = frame.chunk.global_lookup_cache.get(cache_idx as usize);
        if let Some(val) = cache.and_then(|cache| self.get_cached_global(cache)) {
            self.stack.push(val);
            return Ok(());
        }

        let name = str::from_utf8(s).map_err(|_| {
            self.error(ErrorKind::InternalError(
                "compiler emitted non-UTF-8 global name".to_string(),
            ))
        })?;
        let val = if let Some(slot) = crate::instr::Builtin::from_name(name) {
            self.builtins[slot as usize]
        } else if let Some(index) = self.globals.get_index_of(name) {
            if let Some(cache) = cache {
                cache.set(GlobalLookupCacheEntry {
                    globals_version: self.globals_version,
                    index,
                });
            }
            self.globals
                .get_index(index)
                .map(|(_, val)| *val)
                .unwrap_or_default()
        } else {
            Val::Nil
        };
        self.stack.push(val);
        Ok(())
    }

    #[inline(always)]
    fn get_cached_global(&self, cache: &GlobalLookupCacheSlot) -> Option<Val> {
        let entry = cache.get()?;
        if entry.globals_version != self.globals_version {
            return None;
        }
        self.globals.get_index(entry.index).map(|(_, val)| *val)
    }

    /// Fast path for getting well-known builtin globals.
    #[inline(always)]
    fn instr_get_builtin(&mut self, slot: u8) {
        let val = self.builtins[slot as usize];
        self.stack.push(val);
    }

    /// Fast path for setting well-known builtin globals.
    #[inline(always)]
    fn instr_set_builtin(&mut self, slot: u8) {
        let val = self.pop_val();
        self.builtins[slot as usize] = val;
        // Also update globals for _G compatibility
        if let Some(builtin) = crate::instr::Builtin::from_u8(slot) {
            self.globals.insert(builtin.name().to_string(), val);
        }
    }

    #[inline(always)]
    fn instr_get_local(&mut self, local_num: u8) {
        let i = local_num as usize + self.stack_bottom;
        let val = self.stack[i];
        self.stack.push(val);
    }

    fn instr_get_upvalue(&mut self, frame: &Frame, upvalue_num: u8) {
        let uv_ref = frame.upvalues[upvalue_num as usize];
        let val = match self.upvalue_pool.get(uv_ref) {
            Upvalue::Open(stack_idx) => self.stack[*stack_idx],
            Upvalue::Closed(v) => *v,
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

    #[hotpath::measure]
    fn instr_get_table(&mut self) -> Result<()> {
        let key = self.pop_val();
        // Table is now on top of the stack
        let table_idx = self.stack.len() - 1;
        let tbl_val = self.stack[table_idx];
        let obj_ptr = tbl_val.as_object_ptr();
        let (val, has_metatable) = match obj_ptr.and_then(|ptr| self.heap.as_table_ref(ptr)) {
            Some(tbl) => {
                let val = tbl.get(&key);
                (val, tbl.get_metatable().is_some())
            }
            None => {
                let typ = tbl_val.typ_simple();
                self.pop_val();
                return Err(self.type_error(TypeError::TableIndex(typ)));
            }
        };

        if !has_metatable || !matches!(val, Val::Nil) {
            self.stack[table_idx] = val;
            return Ok(());
        }

        self.get_table_with_key(table_idx, key)?;
        // Stack now: [... table, result]
        let result = self.pop_val();
        self.stack[table_idx] = result;
        Ok(())
    }

    #[inline(always)]
    fn remove_stack_pair(&mut self, first: usize) {
        let second_after_pair = first + 2;
        let len = self.stack.len();
        if second_after_pair == len {
            self.stack.truncate(first);
        } else {
            self.stack.copy_within(second_after_pair..len, first);
            self.stack.truncate(len - 2);
        }
    }

    #[inline(always)]
    fn try_insert_table_direct(&mut self, table_idx: usize, key: Val, val: Val) -> Result<bool> {
        let tbl_val = self.stack[table_idx];
        match tbl_val
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table(ptr))
        {
            Some(tbl) => {
                let can_insert_direct =
                    tbl.get_metatable().is_none() || !matches!(tbl.get(&key), Val::Nil);
                if can_insert_direct {
                    tbl.insert(key, val)?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            None => Err(self.type_error(TypeError::TableIndex(tbl_val.typ_simple()))),
        }
    }

    #[hotpath::measure]
    fn instr_init_field(&mut self, frame: &Frame, negative_offset: u8, key_id: u8) -> Result<()> {
        let val = self.pop_val();
        let positive_offset = self.stack.len() - negative_offset as usize - 1;
        let key = self.get_string_constant(frame, key_id);
        let obj_ptr = self.stack[positive_offset].as_object_ptr();
        let typ = self.stack[positive_offset].typ_simple();

        match obj_ptr.and_then(|ptr| self.heap.as_table(ptr)) {
            Some(tbl) => {
                tbl.insert(key, val)?;
                Ok(())
            }
            None => Err(self.error(ErrorKind::InternalError(format!(
                "InitField: expected table, got {typ}"
            )))),
        }
    }

    #[hotpath::measure]
    fn instr_init_index(&mut self, negative_offset: u8) -> Result<()> {
        let val = self.pop_val();
        let key = self.pop_val();
        let positive_offset = self.stack.len() - negative_offset as usize - 1;
        let tbl_typ = self.stack[positive_offset].typ_simple();
        let obj_ptr = self.stack[positive_offset].as_object_ptr();

        match obj_ptr.and_then(|ptr| self.heap.as_table(ptr)) {
            Some(tbl) => {
                tbl.insert(key, val)?;
                Ok(())
            }
            None => Err(self.error(ErrorKind::InternalError(format!(
                "InitIndex: expected table, got {tbl_typ}"
            )))),
        }
    }

    #[hotpath::measure]
    fn instr_length(&mut self) -> Result<()> {
        let val = self.pop_val();

        // Check for string first
        if let Some(s) = val.as_string(&self.heap) {
            let len = s.len();
            self.stack.push(Val::Num(len as f64));
            return Ok(());
        }

        // Check for table
        let obj_ptr = val.as_object_ptr();
        if let Some(ptr) = obj_ptr
            && let Some(tbl) = self.heap.as_table_ref(ptr)
        {
            // Get metatable pointer (Copy, so borrow ends here)
            let mt_ptr = tbl.get_metatable();
            let len = tbl.array_len();
            // Borrow of tbl ends here

            // Check for __len metamethod
            if let Some(mt_ptr) = mt_ptr {
                let len_key = self.alloc_string("__len");
                let len_handler = self
                    .heap
                    .as_table_ref(mt_ptr)
                    .map_or(Val::Nil, |mt| mt.get(&len_key));

                if !matches!(len_handler, Val::Nil) {
                    // Call __len(table)
                    self.stack.push(len_handler);
                    self.stack.push(val);
                    self.call(ArgCount::Fixed(1), RetCount::Fixed(1))?;
                    return Ok(());
                }
            }

            // No __len, use default array_len
            self.stack.push(Val::Num(len as f64));
            return Ok(());
        }

        Err(self.type_error(TypeError::Length(val.typ_simple())))
    }

    #[hotpath::measure]
    fn instr_negate(&mut self) -> Result<()> {
        let n = self.pop_num()?;
        self.stack.push(Val::Num(-n));
        Ok(())
    }

    fn instr_not(&mut self) {
        let b = self.pop_val().truthy();
        self.stack.push(Val::Bool(!b));
    }

    #[hotpath::measure]
    fn instr_set_field(&mut self, frame: &Frame, stack_offset: u8, field_id: u8) -> Result<()> {
        let val = self.pop_val();
        let idx = self.stack.len() - stack_offset as usize - 1;
        let tbl_val = &self.stack[idx];
        let is_table = tbl_val
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .is_some();
        if !is_table {
            let typ = tbl_val.typ_simple();
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
        if let Some(s) = s.as_string(&self.heap) {
            let name = str::from_utf8(s).map_err(|_| {
                self.error(ErrorKind::InternalError(
                    "compiler emitted non-UTF-8 global name".to_string(),
                ))
            })?;
            let name = name.to_owned();
            self.set_global_value_owned(name, val);
            Ok(())
        } else {
            Err(self.error(ErrorKind::InternalError(format!(
                "SetGlobal: expected string constant, got {}",
                s.typ_simple()
            ))))
        }
    }

    #[hotpath::measure]
    fn instr_set_list(&mut self, count: u8) -> Result<usize> {
        // Find the table on the stack (it's below the values)
        // count=0 means "use all values above the table"
        let values = if count == 0 {
            // Find the table - it's the first table value scanning from the bottom of current frame
            let mut table_idx = None;
            for i in self.stack_bottom..self.stack.len() {
                let is_table = self.stack[i]
                    .as_object_ptr()
                    .and_then(|ptr| self.heap.as_table_ref(ptr))
                    .is_some();
                if is_table {
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
        let tbl_value = self.pop_val();
        let obj_ptr = tbl_value.as_object_ptr();
        let typ = tbl_value.typ_simple();

        match obj_ptr.and_then(|ptr| self.heap.as_table(ptr)) {
            Some(tbl) => {
                let n_elements = values.len();
                let counter = 1..;
                for (i, val) in counter.zip(values) {
                    let key = Val::Num(i as f64);
                    tbl.insert(key, val)?;
                }
                self.stack.push(tbl_value);
                Ok(n_elements)
            }
            None => Err(self.error(ErrorKind::InternalError(format!(
                "SetList: expected table, got {typ}"
            )))),
        }
    }

    #[inline(always)]
    fn instr_set_local(&mut self, local_num: u8) {
        let val = self.pop_val();
        let i = local_num as usize + self.stack_bottom;
        self.stack[i] = val;
    }

    #[hotpath::measure]
    fn instr_set_table(&mut self, offset: u8) -> Result<()> {
        let val = self.pop_val();
        let index = self.stack.len() - offset as usize - 2;
        let key = self.stack[index + 1];

        if !self.try_insert_table_direct(index, key, val)? {
            self.set_table_with_key(index, key, val)?;
        }

        self.remove_stack_pair(index);
        Ok(())
    }

    // Helper methods

    /// Compare two values (numbers or strings).
    /// `target` is the ordering we're checking for.
    /// `negate` inverts the result (for <= and >=).
    #[hotpath::measure]
    fn eval_compare(&mut self, target: std::cmp::Ordering, negate: bool) -> Result<()> {
        let v2 = self.pop_val();
        let v1 = self.pop_val();

        let result = match (&v1, &v2) {
            (Val::Num(n1), Val::Num(n2)) => {
                let cmp = n1.partial_cmp(n2).unwrap_or(std::cmp::Ordering::Equal);
                cmp == target
            }
            (Val::Str(s1), Val::Str(s2)) => {
                let cmp = self.heap.get_string(*s1).cmp(self.heap.get_string(*s2));
                cmp == target
            }
            _ => {
                // Type mismatch - error
                return Err(self.error(ErrorKind::TypeError(TypeError::Comparison(
                    v1.typ_simple(),
                    v2.typ_simple(),
                ))));
            }
        };

        self.stack
            .push(Val::Bool(if negate { !result } else { result }));
        Ok(())
    }

    #[inline(always)]
    #[hotpath::measure]
    fn eval_float_float(&mut self, f: impl Fn(f64, f64) -> f64) -> Result<()> {
        let n2 = self.pop_num()?;
        let n1 = self.pop_num()?;
        self.stack.push(Val::Num(f(n1, n2)));
        Ok(())
    }

    fn get_string_constant(&self, frame: &Frame, i: u8) -> Val {
        // self.string_literals[i as usize].clone()
        let index = frame.string_literal_start + i as usize;
        self.string_literals[index]
    }

    fn pop_num(&mut self) -> Result<f64> {
        let val = self.pop_val();
        val.as_num()
            .ok_or_else(|| self.type_error(TypeError::Arithmetic(val.typ_simple())))
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
