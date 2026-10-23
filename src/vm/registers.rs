//! Register forms of the instructions whose work lives in the stack-top
//! helpers.
//!
//! Each handler tries a fast path on the registers directly. When that path
//! cannot finish (a metamethod, a string receiver, a cache miss that needs
//! the general lookup), it copies its operands to the top of the stack,
//! above the frame's registers, and runs the same helper the instruction has
//! always used. Those copies are the instruction's own roots while any
//! callback runs, the helpers already own cost flushing and error cleanup,
//! and the result comes back off the top into the destination register.

use super::super::error::{ConstructorTableOperation, ErrorKind, InternalFault, TypeError};
use super::frame::Frame;
use super::object::Upvalue;
use super::{Error, Result, State, Val};
use crate::instr::{CompareOp, Instr, RetCount};
use crate::limits::MAX_TABLE_BATCH_SIZE;

impl State {
    /// The value of upvalue `index` of the running closure.
    #[inline(always)]
    pub(super) fn upvalue_value(&self, frame: &Frame, index: u16) -> Val {
        let uv_ref = frame.upvalues[usize::from(index)];
        match self.upvalue_pool.get(uv_ref) {
            Upvalue::Open(stack_idx) => self.stack[*stack_idx],
            Upvalue::Closed(v) => *v,
        }
    }

    /// Write upvalue `index` of the running closure.
    #[inline(always)]
    pub(super) fn set_upvalue_value(&mut self, frame: &Frame, index: u16, val: Val) {
        let uv_ref = frame.upvalues[usize::from(index)];
        match self.upvalue_pool.get(uv_ref).clone() {
            Upvalue::Open(stack_idx) => self.stack[stack_idx] = val,
            Upvalue::Closed(_) => *self.upvalue_pool.get_mut(uv_ref) = Upvalue::Closed(val),
        }
    }

    /// Read global `name` through its lookup cache.
    #[inline(always)]
    pub(super) fn read_global(&self, frame: &Frame, name: u16, cache_slot: Option<u8>) -> Val {
        let cache = frame.runtime.caches.global(cache_slot);
        if let Some(val) = cache.and_then(|cache| self.get_cached_global(cache)) {
            return val;
        }
        let name = super::eval_index::verified_global_name(
            &frame.bytecode().string_literals[usize::from(name)],
        );
        self.global_env.read(name, cache)
    }

    /// Write global `name` through its lookup cache.
    #[inline(always)]
    pub(super) fn write_global(
        &mut self,
        frame: &Frame,
        name: u16,
        cache_slot: Option<u8>,
        val: Val,
    ) {
        let cache = frame.runtime.caches.global(cache_slot);
        if let Some(cache) = cache
            && self.global_env.write_cached(cache, val)
        {
            return;
        }
        let name = super::eval_index::verified_global_name(
            &frame.bytecode().string_literals[usize::from(name)],
        );
        self.global_env.write_named(name, val, cache);
    }

    /// Take the value an existing helper left on top into register `dst`.
    #[inline(always)]
    fn pop_into(&mut self, dst: usize) {
        let val = self.pop_val();
        self.stack[dst] = val;
    }

    pub(super) fn reg_new_table_template(
        &mut self,
        frame: &Frame,
        dst: usize,
        template: u16,
    ) -> Result<()> {
        let template = &frame.bytecode().table_templates[usize::from(template)];
        self.new_table_with_template(template, &frame.runtime.literals)?;
        self.pop_into(dst);
        Ok(())
    }

    /// `R[dst] = receiver.name`. Only a current own-field cache hit runs in
    /// the dispatch loop; everything else is outlined, so the hit path does
    /// not carry the frame and spills of the lookup ladder. A hint rather
    /// than `inline(always)`: the dev profile ignores it, which keeps the
    /// probe's unshared debug locals out of `Frame::run`, a frame live across
    /// every native re-entry.
    ///
    /// `call_site` is the dispatch cursor. The outlined stack path, the one
    /// path that can call out, records it first (`State::record_call_site`);
    /// the same holds for the other table and length handlers below. The
    /// record stays out of this function and out of `Frame::run`: here it
    /// tipped the release build into outlining this function, a call on the
    /// hit path, and inlined into the dispatch loop it grew that frame.
    #[inline]
    pub(super) fn reg_get_field(
        &mut self,
        frame: &Frame,
        inst: Instr,
        dst: usize,
        receiver: Val,
        local_cost: &mut u64,
        call_site: usize,
    ) -> Result<()> {
        let cache_slot = inst.cache_slot();
        if let Some(val) = self.field_cache_hit(frame, receiver, cache_slot) {
            self.stack[dst] = val;
            return Ok(());
        }
        // The direct miss returns before the stack path starts, so its frame
        // is not live across the handler calls that path can make.
        if self.get_field_direct(frame, dst, receiver, inst.c(), cache_slot) {
            return Ok(());
        }
        // native-call: reg_get_field->get_field_slow
        self.get_field_slow(frame, inst, dst, receiver, local_cost, call_site)
    }

    #[inline]
    fn field_cache_hit(&self, frame: &Frame, receiver: Val, cache_slot: Option<u8>) -> Option<Val> {
        let cache = frame.runtime.caches.field(cache_slot)?;
        self.get_current_cached_field(receiver.as_object_ptr()?, cache)
    }

    /// The register-native part of a field read that missed the own-field
    /// cache: the raw lookup (which refills that cache, and makes a plain
    /// table's miss nil), then a direct `__index`-table lookup, both without
    /// temporaries or handler calls. Only after the raw lookup has proved the
    /// key absent may the method cache answer: a receiver's version does not
    /// move when it gains a field, so nothing cheaper proves absence. False
    /// leaves chains, handlers and string receivers to the stack path.
    #[inline(never)]
    fn get_field_direct(
        &mut self,
        frame: &Frame,
        dst: usize,
        receiver: Val,
        name: u16,
        cache_slot: Option<u8>,
    ) -> bool {
        let Some(ptr) = receiver.as_object_ptr() else {
            return false;
        };
        let key = frame.literal(name);
        let cache = frame.runtime.caches.field(cache_slot);
        let Some((direct, has_metatable)) = self.get_table_field_direct(ptr, key, cache) else {
            return false;
        };
        let result = match direct {
            Some(result) => result,
            // A miss on a plain table is nil, exactly as `t["k"]` and
            // `rawget` see it.
            None if !has_metatable => Val::Nil,
            None => match self.get_index_table_field_direct(ptr, key, cache) {
                Some(result) => result,
                None => return false,
            },
        };
        self.stack[dst] = result;
        true
    }

    #[inline(never)]
    fn get_field_slow(
        &mut self,
        frame: &Frame,
        inst: Instr,
        dst: usize,
        receiver: Val,
        local_cost: &mut u64,
        call_site: usize,
    ) -> Result<()> {
        self.record_call_site(call_site);
        self.check_stack_space(1)?;
        self.push_unchecked(receiver);
        // native-call: get_field_slow->instr_get_field
        self.instr_get_field(frame, inst.c(), inst.cache_slot(), local_cost)?;
        self.pop_into(dst);
        Ok(())
    }

    /// `R[dst] = R[table][R[key]]`.
    pub(super) fn reg_get_table(
        &mut self,
        dst: usize,
        table: usize,
        key: usize,
        local_cost: &mut u64,
        call_site: usize,
    ) -> Result<()> {
        let tbl_val = self.stack[table];
        let key_val = self.stack[key];
        if let Some(tbl) = tbl_val
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
        {
            let val = tbl.get(&key_val);
            if tbl.get_metatable().is_none() || !matches!(val, Val::Nil) {
                self.stack[dst] = val;
                return Ok(());
            }
        }
        // native-call: reg_get_table->get_table_slow
        self.get_table_slow(dst, tbl_val, key_val, local_cost, call_site)
    }

    #[inline(never)]
    fn get_table_slow(
        &mut self,
        dst: usize,
        table: Val,
        key: Val,
        local_cost: &mut u64,
        call_site: usize,
    ) -> Result<()> {
        self.record_call_site(call_site);
        self.check_stack_space(2)?;
        self.push_unchecked(table);
        self.push_unchecked(key);
        // native-call: get_table_slow->instr_get_table
        self.instr_get_table(local_cost)?;
        self.pop_into(dst);
        Ok(())
    }

    /// `R[table].name = R[src]`.
    pub(super) fn reg_set_field(
        &mut self,
        frame: &Frame,
        inst: Instr,
        table: usize,
        src: usize,
        local_cost: &mut u64,
        call_site: usize,
    ) -> Result<()> {
        let (name, cache_slot) = (inst.b(), inst.cache_slot());
        let receiver = self.stack[table];
        let val = self.stack[src];
        if !matches!(val, Val::Nil)
            && let Some(ptr) = receiver.as_object_ptr()
        {
            let key = frame.literal(name);
            let cache = frame.runtime.caches.set_field(cache_slot);
            if let Some(cache) = cache
                && self.try_set_field_cached(ptr, key, val, cache)
            {
                return Ok(());
            }
            if self.try_set_field_direct(ptr, key, val, cache) {
                return Ok(());
            }
        }
        // native-call: reg_set_field->set_field_slow
        self.set_field_slow(frame, inst, receiver, val, local_cost, call_site)
    }

    #[inline(never)]
    fn set_field_slow(
        &mut self,
        frame: &Frame,
        inst: Instr,
        receiver: Val,
        val: Val,
        local_cost: &mut u64,
        call_site: usize,
    ) -> Result<()> {
        self.record_call_site(call_site);
        self.check_stack_space(2)?;
        self.push_unchecked(receiver);
        self.push_unchecked(val);
        // native-call: set_field_slow->instr_set_field
        self.instr_set_field(frame, 0, inst.b(), inst.cache_slot(), local_cost)
    }

    /// `R[table][R[key]] = R[src]`.
    pub(super) fn reg_set_table(
        &mut self,
        table: usize,
        key: usize,
        src: usize,
        local_cost: &mut u64,
        call_site: usize,
    ) -> Result<()> {
        let key_val = self.stack[key];
        let val = self.stack[src];
        if self.try_insert_table_direct(table, key_val, val)? {
            return Ok(());
        }
        let tbl_val = self.stack[table];
        // native-call: reg_set_table->set_table_slow
        self.set_table_slow(tbl_val, key_val, val, local_cost, call_site)
    }

    #[inline(never)]
    fn set_table_slow(
        &mut self,
        table: Val,
        key: Val,
        val: Val,
        local_cost: &mut u64,
        call_site: usize,
    ) -> Result<()> {
        self.record_call_site(call_site);
        self.check_stack_space(3)?;
        self.push_unchecked(table);
        self.push_unchecked(key);
        self.push_unchecked(val);
        // native-call: set_table_slow->instr_set_table
        self.instr_set_table(0, local_cost)
    }

    /// The constructor table in register `table`. Verification proves
    /// register ranges, not value types, so the check stays fallible.
    fn constructor_table(
        &mut self,
        table: usize,
        operation: ConstructorTableOperation,
    ) -> Result<&mut super::table::Table> {
        let tbl_val = self.stack[table];
        let typ = tbl_val.typ(&self.heap);
        match tbl_val.as_object_ptr() {
            Some(ptr) if self.heap.as_table_ref(ptr).is_some() => Ok(self
                .heap
                .as_table(ptr)
                .expect("checked to be a table just above")),
            _ => Err(Error::internal_fault(
                InternalFault::ConstructorTableMissing {
                    operation,
                    actual: typ,
                },
            )),
        }
    }

    /// A constructor's `name = R[src]`.
    pub(super) fn reg_init_field(
        &mut self,
        frame: &Frame,
        table: usize,
        name: u16,
        src: usize,
    ) -> Result<()> {
        let key = frame.literal(name);
        let val = self.stack[src];
        self.constructor_table(table, ConstructorTableOperation::InitField)?
            .insert(key, val)?;
        Ok(())
    }

    /// A template constructor's `name = R[src]` at its pinned entry.
    pub(super) fn reg_init_field_pinned(
        &mut self,
        frame: &Frame,
        table: usize,
        name: u16,
        src: usize,
        entry: u8,
    ) -> Result<()> {
        let key = frame.literal(name);
        let val = self.stack[src];
        let tbl = self.constructor_table(table, ConstructorTableOperation::InitFieldPinned)?;
        if !tbl.init_at_index(usize::from(entry), key, val) {
            tbl.insert(key, val)?;
        }
        Ok(())
    }

    /// A constructor's `[R[key]] = R[src]`.
    pub(super) fn reg_init_index(&mut self, table: usize, key: usize, src: usize) -> Result<()> {
        let key_val = self.stack[key];
        let val = self.stack[src];
        self.constructor_table(table, ConstructorTableOperation::InitIndex)?
            .insert(key_val, val)?;
        Ok(())
    }

    /// Store `count` positional values from `R[table + 1..]` as batch
    /// `batch` of the constructor in `R[table]`.
    pub(super) fn reg_set_list(&mut self, table: usize, count: usize, batch: u16) -> Result<()> {
        let first_key = usize::from(batch) * usize::from(MAX_TABLE_BATCH_SIZE) + 1;
        let tbl_val = self.stack[table];
        let typ = tbl_val.typ(&self.heap);
        let Some(ptr) = tbl_val
            .as_object_ptr()
            .filter(|ptr| self.heap.as_table_ref(*ptr).is_some())
        else {
            return Err(Error::internal_fault(
                InternalFault::ConstructorTableMissing {
                    operation: ConstructorTableOperation::SetList,
                    actual: typ,
                },
            ));
        };
        // Nothing in `set_list` can enter the collector or a callback, so
        // the values are read in place.
        let values = &self.stack[table + 1..table + 1 + count];
        let tbl = self
            .heap
            .as_table(ptr)
            .expect("checked to be a table just above");
        tbl.set_list(first_key, values.iter().copied())?;
        Ok(())
    }

    /// The error for a non-number arithmetic operand, the right one checked
    /// first.
    #[cold]
    #[inline(never)]
    pub(super) fn arithmetic_operands_error(&self, lhs: Val, rhs: Val) -> Error {
        let bad = if rhs.as_num().is_none() { rhs } else { lhs };
        self.type_error(TypeError::Arithmetic(bad.typ(&self.heap)))
    }

    /// `R[dst] = #R[src]`.
    pub(super) fn reg_len(
        &mut self,
        dst: usize,
        src: usize,
        local_cost: &mut u64,
        call_site: usize,
    ) -> Result<()> {
        let val = self.stack[src];
        if let Some(s) = val.as_string(&self.heap) {
            self.stack[dst] = Val::Num(s.len() as f64);
            return Ok(());
        }
        if let Some(tbl) = val
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            && tbl.get_metatable().is_none()
        {
            self.stack[dst] = Val::Num(tbl.array_len() as f64);
            return Ok(());
        }
        // native-call: reg_len->len_slow
        self.len_slow(dst, val, local_cost, call_site)
    }

    #[inline(never)]
    fn len_slow(
        &mut self,
        dst: usize,
        val: Val,
        local_cost: &mut u64,
        call_site: usize,
    ) -> Result<()> {
        self.record_call_site(call_site);
        self.check_stack_space(1)?;
        self.push_unchecked(val);
        // native-call: len_slow->instr_length
        self.instr_length(local_cost)?;
        self.pop_into(dst);
        Ok(())
    }

    /// `R[dst] = R[first] .. ... .. R[first + count - 1]`.
    pub(super) fn reg_concat(&mut self, dst: usize, first: usize, count: usize) -> Result<()> {
        self.check_stack_space(count)?;
        self.stack.extend_from_within(first..first + count);
        self.concat_helper(count)?;
        self.pop_into(dst);
        Ok(())
    }

    /// Compare two values. Ordering forms take numbers or strings; an
    /// incomparable pair (NaN involved) compares false before negation.
    /// Equality forms compare raw.
    #[inline(always)]
    pub(super) fn compare_vals(&self, op: CompareOp, lhs: Val, rhs: Val) -> Result<bool> {
        let Some((target, negate)) = op.ordering() else {
            return Ok((lhs == rhs) == (op == CompareOp::Eq));
        };
        let result = match (lhs, rhs) {
            (Val::Num(n1), Val::Num(n2)) => n1.partial_cmp(&n2).map(|cmp| cmp == target),
            (Val::Str(s1), Val::Str(s2)) => {
                let cmp = self.heap.get_string(s1).cmp(self.heap.get_string(s2));
                Some(cmp == target)
            }
            _ => {
                return Err(self.error(ErrorKind::TypeError(TypeError::Comparison(
                    lhs.typ(&self.heap),
                    rhs.typ(&self.heap),
                ))));
            }
        };
        Ok(result.is_some_and(|result| if negate { !result } else { result }))
    }

    /// `R[dst] = closure of nested function `nested``.
    pub(super) fn reg_closure(&mut self, frame: &Frame, dst: usize, nested: u16) -> Result<()> {
        self.instr_closure(frame, nested)?;
        self.pop_into(dst);
        Ok(())
    }

    /// Copy varargs to `R[dst..]`: exactly `n` padded with nil, or every one,
    /// leaving the top after the last for the consumer that follows.
    pub(super) fn reg_vararg(&mut self, dst: usize, count: RetCount) -> Result<()> {
        // While innermost, the activation owns the arena suffix.
        let start = self
            .call_stack
            .last()
            .expect("an executing frame has an activation record")
            .vararg_start as usize;
        let available = self.vararg_stack.len() - start;
        match count {
            RetCount::Fixed(n) => {
                let n = usize::from(n);
                let copied = n.min(available);
                if copied > 0 {
                    let varargs = &self.vararg_stack[start..];
                    self.stack[dst..dst + copied].copy_from_slice(&varargs[..copied]);
                }
                self.stack[dst + copied..dst + n].fill(Val::Nil);
            }
            RetCount::All => {
                self.check_window(dst)?;
                // Replacing the rest of the frame may leave less room than
                // the current stack length suggests. Check the prospective
                // top before changing the live registers.
                let additional = available.saturating_sub(self.stack.len().saturating_sub(dst));
                self.check_stack_space(additional)?;
                self.stack.truncate(dst);
                if available > 0 {
                    let varargs = &self.vararg_stack[start..];
                    self.stack.extend_from_slice(varargs);
                }
            }
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "fault-injection"))]
mod tests {
    use std::sync::Arc;

    use super::{RetCount, State, Val};
    use crate::compiler::Bytecode;
    use crate::error::ErrorKind;
    use crate::vm::CallInfo;

    #[test]
    fn refused_open_vararg_growth_preserves_the_live_register_window() {
        let mut state = State::new();
        let original = vec![
            Val::Num(10.0),
            Val::Num(20.0),
            Val::Num(30.0),
            Val::Num(40.0),
        ];
        state.stack.clone_from(&original);
        state.vararg_stack.extend_from_slice(&[
            Val::Num(50.0),
            Val::Num(60.0),
            Val::Num(70.0),
            Val::Num(80.0),
        ]);
        state.call_stack.push(CallInfo {
            bytecode: Arc::new(Bytecode::default()),
            ip: 0,
            vararg_start: 0,
            chain_charges: 0,
        });
        state.arm_fault_injection(Some(1));

        let error = state
            .reg_vararg(1, RetCount::All)
            .expect_err("the open vararg preflight is refused");

        assert!(matches!(error.kind(), ErrorKind::StackOverflow { .. }));
        assert_eq!(state.stack, original);
        assert_eq!(
            state.finish_fault_injection(),
            vec![("stack_space", 1, true)]
        );
    }
}
