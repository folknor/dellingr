use super::super::compiler::UpvalueDesc;
use super::super::error::TypeError;
use super::Result;
use super::State;
use super::Val;
use super::caches::{RuntimeCaches, TforCursorEntry, TforCursorSlot};
use super::frame::Frame;
use super::lua_val::RustFunc;
use crate::instr::{ArgCount, RetCount};
use crate::lua_std::{base_ipairs_iter, base_next};

impl State {
    /// Push a closure of nested function `i`, capturing its upvalues from
    /// the running frame.
    #[hotpath::measure]
    pub(super) fn instr_closure(&mut self, frame: &Frame, i: u16) -> Result<()> {
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
        self.push_closure(bytecode, captured_upvalues)
    }

    /// Numeric-for setup over the registers from `slot`: start, limit and
    /// step, then the visible variable. Checks the step, then the limit, then
    /// the start, and moves the dispatch cursor `ip` past the loop when it
    /// does not run.
    #[hotpath::measure]
    pub(super) fn instr_for_prep(
        &mut self,
        slot: usize,
        ip: &mut usize,
        body_len: i32,
    ) -> Result<()> {
        let start_val = self.stack[slot];
        let end_val = self.stack[slot + 1];
        let step_val = self.stack[slot + 2];
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
            for (local_slot, n) in (slot..).zip([start, end, step, start]) {
                self.stack[local_slot] = Val::Num(n);
            }
        } else {
            *ip = Frame::jumped(*ip, body_len);
        }
        Ok(())
    }

    /// Numeric-for step; moves the dispatch cursor `ip` back to the body
    /// while the loop continues.
    #[hotpath::measure]
    pub(super) fn instr_for_loop(
        &mut self,
        slot: usize,
        ip: &mut usize,
        offset: i32,
    ) -> Result<()> {
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
            *ip = Frame::jumped(*ip, offset);
        }
        Ok(())
    }

    /// TForCall: Call iterator(state, control) from the registers at `base`,
    /// store results in the loop variable registers after them.
    #[hotpath::measure]
    pub(super) fn instr_tfor_call(
        &mut self,
        base: usize,
        num_vars: u8,
        cursor_slot: Option<u8>,
        caches: &RuntimeCaches,
    ) -> Result<()> {
        // Push iterator function, state, and control onto stack for call
        let iterator = self.stack[base];
        let state = self.stack[base + 1];
        let control = self.stack[base + 2];

        if let Val::RustFn(f) = iterator {
            // Identity is by function address (the same contract as `Val`
            // equality and snapshot naming). The fast paths are semantically
            // transparent: if identical-code folding made another function
            // share these addresses, it would be identical code anyway.
            let base_next_fn: RustFunc = base_next;
            let base_ipairs_iter_fn: RustFunc = base_ipairs_iter;
            if std::ptr::fn_addr_eq(f, base_next_fn)
                && self.instr_tfor_call_next(base, state, control, num_vars, cursor_slot, caches)
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

        self.check_stack_space(3)?;
        self.push_unchecked(iterator);
        self.push_unchecked(state);
        self.push_unchecked(control);

        // Call with 2 args (state, control), expecting num_vars returns
        self.call_unguarded(ArgCount::Fixed(2), RetCount::Fixed(num_vars))?;

        // The call preserves caller slot positions (see call_unguarded); user code
        // may change captured values, but cannot remove these loop slots.
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
        cursor_slot: Option<u8>,
        caches: &RuntimeCaches,
    ) -> bool {
        let Some(table_ptr) = state.as_object_ptr() else {
            return false;
        };
        let Some(tbl) = self.heap.as_table_ref(table_ptr) else {
            return false;
        };

        let cursor = caches.tfor_cursor(cursor_slot);
        let next = match cursor.and_then(TforCursorSlot::get) {
            Some(entry) if entry.table == table_ptr => {
                match tbl.next_from_matching_index(entry.index, &control) {
                    super::table::TableNextWithIndex::InvalidKey => tbl.next_with_index(&control),
                    next => next,
                }
            }
            _ => tbl.next_with_index(&control),
        };
        match next {
            super::table::TableNextWithIndex::Pair { index, key, value } => {
                if let Some(slot) = cursor {
                    slot.set(TforCursorEntry {
                        table: table_ptr,
                        index,
                    });
                }
                self.write_tfor_results(base, num_vars, key, Some(value));
            }
            super::table::TableNextWithIndex::End => {
                self.write_tfor_results(base, num_vars, Val::Nil, None);
            }
            super::table::TableNextWithIndex::InvalidKey => return false,
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
        let call_base = self.stack.len();

        self.check_stack_space(2)?;
        self.push_unchecked(state);
        self.push_unchecked(control);

        let num_ret_reported = self.invoke_rust_fn(f, call_base, false)?;

        // This truncate is redundant: the error propagates into
        // `unwind_invocation`, which truncates the whole frame. It stays
        // because this function inlines into `Frame::run`, and deleting it
        // re-packs that frame's spill slots one 16-byte alignment step larger
        // in release, on every native re-entry cycle (measured with
        // `tests/native_stack.rs`'s `native_bytes_per_level`). Keep it until
        // a build shows the frame holds without it.
        if let Err(e) = self.balance_stack(num_vars as usize, num_ret_reported as usize) {
            self.stack.truncate(call_base);
            return Err(e);
        }
        let results_start = self.stack.len() - num_vars as usize;
        for i in 0..num_vars as usize {
            self.stack[base + 3 + i] = self.stack[results_start + i];
        }
        self.stack.truncate(results_start);

        Ok(())
    }

    /// TForLoop: if the first loop variable is nil, move the dispatch cursor
    /// `ip` out of the loop. Otherwise update the control variable.
    #[hotpath::measure]
    pub(super) fn instr_tfor_loop(&mut self, base: usize, ip: &mut usize, offset: i32) {
        let first_var = &self.stack[base + 3];

        if matches!(first_var, Val::Nil) {
            // Exit loop
            *ip = Frame::jumped(*ip, offset);
        } else {
            // Update control variable with first loop variable
            self.stack[base + 2] = self.stack[base + 3];
        }
    }
}

fn check_numeric_for_condition(var: f64, limit: f64, step: f64) -> bool {
    // Deliberate divergence: a zero step (including -0.0) skips the loop in
    // both directions. Lua 5.4 raises "'for' step is zero"; Lua 5.2 skips an
    // ascending range but loops forever on a descending one. The skip rules
    // out the unbounded-execution case without killing the callback the way
    // the 5.4 error would. Documented in docs/compatibility.md; pinned by
    // examples/for_zero_step.lua (marked DIFF).
    if step == 0.0 {
        false
    } else if step > 0.0 {
        var <= limit
    } else {
        // step < 0.0
        var >= limit
    }
}
