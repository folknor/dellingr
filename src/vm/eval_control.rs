use super::super::compiler::UpvalueDesc;
use super::super::error::TypeError;
use super::Result;
use super::State;
use super::Val;
use super::frame::Frame;
use super::lua_val::RustFunc;
use crate::instr::{ArgCount, RetCount};
use crate::lua_std::{base_ipairs_iter, base_next};

impl State {
    /// Pop a value. If its truthiness matches `cond`, jump with `offset`.
    /// If `keep_cond`, then push the value back after jumping.
    #[hotpath::measure]
    pub(super) fn instr_branch(
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
    pub(super) fn instr_closure(&mut self, frame: &mut Frame, i: u8) {
        let bytecode = frame.get_nested_bytecode(i);
        // Capture upvalues based on the bytecode's upvalue descriptors
        let mut captured_upvalues = Vec::with_capacity(bytecode.upvalues.len());
        for desc in &bytecode.upvalues {
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
        self.push_closure(bytecode, captured_upvalues);
    }

    #[hotpath::measure]
    pub(super) fn instr_for_prep(
        &mut self,
        frame: &mut Frame,
        local: u8,
        body_len: i16,
    ) -> Result<()> {
        let step_val = self.pop_val();
        let end_val = self.pop_val();
        let start_val = self.pop_val();
        let step = step_val
            .as_num()
            .ok_or_else(|| self.type_error(TypeError::Arithmetic(step_val.typ(&self.heap))))?;
        let end = end_val
            .as_num()
            .ok_or_else(|| self.type_error(TypeError::Arithmetic(end_val.typ(&self.heap))))?;
        let start = start_val
            .as_num()
            .ok_or_else(|| self.type_error(TypeError::Arithmetic(start_val.typ(&self.heap))))?;
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
    pub(super) fn instr_for_loop(
        &mut self,
        frame: &mut Frame,
        local_slot: u8,
        offset: i16,
    ) -> Result<()> {
        let slot = local_slot as usize + self.stack_bottom;
        let mut var = self.stack[slot].as_num().ok_or_else(|| {
            self.type_error(TypeError::Arithmetic(self.stack[slot].typ(&self.heap)))
        })?;
        let limit = self.stack[slot + 1].as_num().ok_or_else(|| {
            self.type_error(TypeError::Arithmetic(self.stack[slot + 1].typ(&self.heap)))
        })?;
        let step = self.stack[slot + 2].as_num().ok_or_else(|| {
            self.type_error(TypeError::Arithmetic(self.stack[slot + 2].typ(&self.heap)))
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
    pub(super) fn instr_tfor_prep(&mut self, local_slot: u8) {
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
    pub(super) fn instr_tfor_call(&mut self, local_slot: u8, num_vars: u8) -> Result<()> {
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
    pub(super) fn write_tfor_results(
        &mut self,
        base: usize,
        num_vars: u8,
        first: Val,
        second: Option<Val>,
    ) {
        for i in 0..num_vars as usize {
            self.stack[base + 3 + i] = match i {
                0 => first,
                1 => second.unwrap_or(Val::Nil),
                _ => Val::Nil,
            };
        }
    }

    pub(super) fn instr_tfor_call_next(
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

        match tbl.next(&control) {
            super::table::TableNext::Pair(next_key, next_val) => {
                self.write_tfor_results(base, num_vars, next_key, Some(next_val));
            }
            super::table::TableNext::End => {
                self.write_tfor_results(base, num_vars, Val::Nil, None);
            }
            super::table::TableNext::InvalidKey => return false,
        }
        true
    }

    pub(super) fn instr_tfor_call_ipairs(
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

    pub(super) fn instr_tfor_call_rust_fn(
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
    pub(super) fn instr_tfor_loop(
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
            self.stack[base + 2] = self.stack[base + 3];
        }
        Ok(())
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
