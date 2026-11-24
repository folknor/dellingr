use super::super::error::{Error, InternalFault, TypeError};
use super::Result;
use super::RustFnId;
use super::State;
use super::Val;
use super::caches::{RuntimeCaches, TforCursorEntry, TforCursorSlot};
use super::frame::Frame;
use crate::instr::{ArgCount, RetCount};

impl State {
    /// Numeric-for setup over the registers from `slot`: start, limit and
    /// step, then the visible variable. Checks the step, then the limit, then
    /// the start, and moves the dispatch cursor `ip` past the loop when it
    /// does not run. A loop that runs and has a numeric region
    /// (`vm/numeric_loop.rs`) continues there, which returns with `ip` past
    /// the loop, or unchanged for canonical dispatch to run the body.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(super) fn instr_for_prep(
        &mut self,
        frame: &Frame,
        slot: usize,
        ip: &mut usize,
        body_len: i32,
        local_cost: &mut u64,
    ) -> Result<()> {
        let start_val = self.stack[slot];
        let end_val = self.stack[slot + 1];
        let step_val = self.stack[slot + 2];
        let step = step_val
            .as_num()
            .ok_or_else(|| self.type_error(TypeError::NumericForStep(step_val.typ(&self.heap))))?;
        let end = end_val
            .as_num()
            .ok_or_else(|| self.type_error(TypeError::NumericForLimit(end_val.typ(&self.heap))))?;
        let start = start_val.as_num().ok_or_else(|| {
            self.type_error(TypeError::NumericForInitialValue(start_val.typ(&self.heap)))
        })?;
        if check_numeric_for_condition(start, end, step) {
            for (local_slot, n) in (slot..).zip([start, end, step, start]) {
                self.stack[local_slot] = Val::Num(n);
            }
            if let Some(region) = frame.runtime.numeric_loops.get(*ip - 1) {
                self.run_numeric_loop(region, frame, ip, local_cost)?;
            }
        } else {
            // Admission requires a forward offset here, so this jump is
            // never an interrupt safepoint.
            *ip = Frame::jumped(*ip, body_len);
        }
        Ok(())
    }

    /// Numeric-for step; moves the dispatch cursor `ip` back to the body
    /// while the loop continues.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(super) fn instr_for_loop(
        &mut self,
        slot: usize,
        ip: &mut usize,
        offset: i32,
    ) -> Result<()> {
        // Compiled bodies cannot write these hidden controls, but admission
        // proves register bounds, not setup dominance or runtime value types.
        // Edited snapshot bytecode may skip setup or overwrite a control.
        // This fallible match stays even under a stronger admission proof:
        // the proof is invisible to the `Val` payload read, and the panic it
        // could license is what admitted bytecode must never reach. Faster
        // numeric loops come from the typed loop region (`vm/numeric_loop.rs`)
        // holding the controls as `f64` locals, selected at FOR_PREP by an
        // entry guard rather than a static proof, with this handler as the
        // fallback.
        let (Some(mut var), Some(limit), Some(step)) = (
            self.stack[slot].as_num(),
            self.stack[slot + 1].as_num(),
            self.stack[slot + 2].as_num(),
        ) else {
            return Err(invalid_numeric_for_controls());
        };
        var += step;
        if check_numeric_for_condition(var, limit, step) {
            // Jump first: the jump back is an interrupt safepoint, and an
            // interrupted iteration writes no counter.
            Frame::branch(self, ip, offset)?;
            self.stack[slot] = Val::Num(var);
            self.stack[slot + 3] = Val::Num(var);
        }
        Ok(())
    }

    /// The generic-for step for `next` and the `ipairs` iterator, done in
    /// the dispatch loop: true when it wrote the loop variables, false when
    /// the step needs the iterator call (`instr_tfor_call`). It recognizes
    /// the catalog entries of `next` and the `ipairs` iterator, the same
    /// identity `Val` equality and snapshot naming use. The fast paths are
    /// semantically transparent: each does what its entry's function does.
    /// They invoke no iterator or callback, read no meter, record no call
    /// site and cannot fail, so the step needs neither a call-site record nor a
    /// published cost batch.
    ///
    /// Forced inline only when debug assertions are off: measured in
    /// `native_bytes_per_level` cycles, forcing it cost the dev build 48
    /// bytes per cycle and not forcing it cost the release build 16.
    #[cfg_attr(not(debug_assertions), inline(always))]
    pub(super) fn tfor_fast_step(
        &mut self,
        base: usize,
        num_vars: u8,
        cursor_slot: Option<u8>,
        caches: &RuntimeCaches,
    ) -> bool {
        let Val::RustFn(f) = self.stack[base] else {
            return false;
        };
        let state = self.stack[base + 1];
        let control = self.stack[base + 2];
        if f == crate::lua_std::NEXT {
            return self.instr_tfor_call_next(base, state, control, num_vars, cursor_slot, caches);
        }
        if f == crate::lua_std::IPAIRS_ITER {
            return self.instr_tfor_call_ipairs(base, state, control, num_vars);
        }
        false
    }

    /// TForCall: Call iterator(state, control) from the registers at `base`,
    /// store results in the loop variable registers after them. The
    /// dispatch arm tries `tfor_fast_step` first, then records the call site
    /// and publishes the cost batch before this runs.
    pub(super) fn instr_tfor_call(&mut self, base: usize, num_vars: u8) -> Result<()> {
        // Push iterator function, state, and control onto stack for call
        let iterator = self.stack[base];
        let state = self.stack[base + 1];
        let control = self.stack[base + 2];

        // A Rust iterator is called on (state, control); a gmatch iterator
        // runs its step with itself as the first argument (`GmatchIter`),
        // the state riding along unread. Neither enters `call_unguarded`.
        let rust_call = match iterator {
            Val::RustFn(f) => Some((f, state, control)),
            _ => self
                .gmatch_step_of(iterator)
                .map(|step| (step, iterator, state)),
        };
        if let Some((f, first, second)) = rust_call {
            return crate::native_call!(
                "instr_tfor_call->instr_tfor_call_rust_fn",
                self.instr_tfor_call_rust_fn(f, base, first, second, num_vars)
            );
        }

        let call_base = self.stack.len();
        self.check_stack_space(3)?;
        self.push_unchecked(iterator);
        self.push_unchecked(state);
        self.push_unchecked(control);

        // Call with 2 args (state, control) and take every result: they sit
        // in storage the call already checked (a Lua callee's own frame, or
        // what a Rust callee pushed), where padding to `num_vars` above the
        // caller's frame could fail the stack cap after a successful call.
        crate::native_call!(
            "instr_tfor_call->call_unguarded",
            self.call_unguarded(ArgCount::Fixed(2), RetCount::All)
        )?;
        let returned = self.stack.len() - call_base;
        self.take_tfor_results(base, num_vars, call_base, returned);
        Ok(())
    }

    /// Move the `returned` iterator results at `call_base` into the loop
    /// variables (`base + 3..`), dropping extra results and setting missing
    /// ones to nil in the registers themselves, then drop the results. The
    /// call preserves the caller's slot positions (see `call_unguarded`);
    /// user code may change captured values, but cannot remove these loop
    /// slots.
    #[inline(always)]
    fn take_tfor_results(&mut self, base: usize, num_vars: u8, call_base: usize, returned: usize) {
        for i in 0..num_vars as usize {
            self.stack[base + 3 + i] = if i < returned {
                self.stack[call_base + i]
            } else {
                Val::Nil
            };
        }
        self.stack.truncate(call_base);
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
        let hint = match cursor.and_then(TforCursorSlot::get) {
            Some(entry) if entry.table == table_ptr => Some(entry.index),
            _ => None,
        };
        let next = tbl.next_with_index(&control, hint);
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
        let (Some(old_index), Some(table)) = (control.as_num(), state.as_object_ptr()) else {
            return false;
        };
        match self.ipairs_step(table, old_index) {
            IpairsStep::Yield(key, value) => {
                self.write_tfor_results(base, num_vars, Val::Num(key), Some(value));
            }
            IpairsStep::Exhausted => self.write_tfor_results(base, num_vars, Val::Nil, None),
            IpairsStep::Fallback => return false,
        }
        true
    }

    /// One step of the `ipairs` iterator over `table` from `control`, as the
    /// iterator callback makes it, for the steps that need no callback: the
    /// one rule the dispatch loop's fast step and the typed ipairs region
    /// (`vm/numeric_loop.rs`) share. `Fallback` leaves the step to the
    /// callback: a non-table state, an invalid control (the callback raises
    /// the error), or a raw nil on a table with a metatable (`__index`).
    #[inline(always)]
    pub(super) fn ipairs_step(&self, table: super::object::ObjectPtr, control: f64) -> IpairsStep {
        let Some(tbl) = self.heap.as_table_ref(table) else {
            return IpairsStep::Fallback;
        };
        let Some(key) = crate::lua_std::ipairs_next_index(control) else {
            return IpairsStep::Fallback;
        };
        match tbl.get_number(key) {
            Val::Nil if tbl.get_metatable().is_some() => IpairsStep::Fallback,
            Val::Nil => IpairsStep::Exhausted,
            value => IpairsStep::Yield(key, value),
        }
    }

    pub(super) fn instr_tfor_call_rust_fn(
        &mut self,
        f: RustFnId,
        base: usize,
        state: Val,
        control: Val,
        num_vars: u8,
    ) -> Result<()> {
        let call_base = self.stack.len();

        self.check_stack_space(2)?;
        self.push_unchecked(state);
        self.push_unchecked(control);
        let num_ret_reported = crate::native_call!(
            "instr_tfor_call_rust_fn->invoke_rust_fn",
            self.invoke_rust_fn(f, call_base, false, ())
        )?;
        // The callback's results are at `call_base`, in slots it pushed.
        self.take_tfor_results(base, num_vars, call_base, usize::from(num_ret_reported));
        Ok(())
    }
}

/// What one `ipairs` step does without its callback (`State::ipairs_step`).
pub(super) enum IpairsStep {
    /// The next key and its non-nil value: the loop continues.
    Yield(f64, Val),
    /// A raw nil on a table without a metatable: the loop ends.
    Exhausted,
    /// The callback must make the step.
    Fallback,
}

pub(super) fn check_numeric_for_condition(var: f64, limit: f64, step: f64) -> bool {
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

/// Keep malformed-bytecode error construction out of the loop's fast path.
#[cold]
#[inline(never)]
fn invalid_numeric_for_controls() -> Error {
    Error::internal_fault(InternalFault::NumericForControlsNotNumeric)
}
