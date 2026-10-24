//! Register forms of the instructions that do more than move values.
//!
//! Every handler works on the registers in place: operands are read from
//! their registers (or the literal pool) and the result is written straight
//! into the destination register. Nothing is copied above the frame. The
//! table and length instructions try an inline fast path, then an outlined
//! resolver that calls nothing (`State::resolve_index`,
//! `State::store_field`, `State::resolve_length`); only when that resolver
//! ends at a function handler, with the call window pushed, does a small
//! outlined call function (`index_call`, `newindex_call`, `len_call`) make
//! the call. The window is the only stack an instruction uses above its
//! frame, and admission reserves room for the largest one
//! (`State::admit_lua_call`), so none of these instructions can fail the
//! stack cap itself.

use super::super::error::{ConstructorTableOperation, ErrorKind, InternalFault, TypeError};
use super::frame::Frame;
use super::metamethod::{IndexOutcome, IndexStart};
use super::object::Upvalue;
use super::{Error, Result, State, Val};
use crate::compiler::UpvalueDesc;
use crate::instr::{ArgCount, CompareOp, Instr, RetCount};
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
        if let Some(val) = cache.and_then(|cache| self.global_env.read_cached(&self.heap, cache)) {
            return val;
        }
        self.global_env.read(&self.heap, frame.literal(name), cache)
    }

    /// Write register `src` to global `name` through its lookup cache. The
    /// cached store reads the register in place (see
    /// `GlobalEnv::write_cached` for why it takes a reference).
    #[inline(always)]
    pub(super) fn write_global(
        &mut self,
        frame: &Frame,
        name: u16,
        cache_slot: Option<u8>,
        src: usize,
    ) {
        let cache = frame.runtime.caches.global(cache_slot);
        let val = &self.stack[src];
        if let Some(cache) = cache
            && self.global_env.write_cached(&mut self.heap, cache, val)
        {
            return;
        }
        self.global_env
            .write(&mut self.heap, frame.literal(name), *val, cache);
    }

    /// Take a handler's one result off the top into register `dst`.
    #[inline(always)]
    fn pop_into(&mut self, dst: usize) {
        let val = self.pop_val();
        self.stack[dst] = val;
    }

    /// `R[dst] = {...}` from a constructor template. The registers are roots
    /// while the allocator may collect, and the new table goes straight into
    /// its register, as `OP_NEW_TABLE`'s does. Outlined, so the template
    /// table's construction stays out of `Frame::run`.
    #[hotpath::measure]
    #[inline(never)]
    pub(super) fn reg_new_table_template(&mut self, frame: &Frame, dst: usize, template: u16) {
        let template = &frame.bytecode().table_templates[usize::from(template)];
        let obj = self
            .allocator()
            .alloc_table_with_template(template, &frame.runtime.literals);
        self.stack[dst] = Val::Obj(obj);
    }

    /// `R[dst] = receiver.name`. Only a current own-field cache hit runs in
    /// the dispatch loop; everything else is outlined, so the hit path does
    /// not carry the frame and spills of the lookup ladder. A hint rather
    /// than `inline(always)`: the dev profile ignores it, which keeps the
    /// probe's unshared debug locals out of `Frame::run`, a frame live across
    /// every native re-entry.
    ///
    /// Everything past the hit is one outlined resolver call, which reads
    /// each table once and returns before any handler runs; only a function
    /// handler continues into `index_call`.
    ///
    /// `call_site` is the dispatch cursor. The call function, the one path
    /// that can call out, records it first (`State::record_call_site`); the
    /// same holds for the other table and length handlers below. The record
    /// stays out of this function and out of `Frame::run`: here it tipped
    /// the release build into outlining this function, a call on the hit
    /// path, and inlined into the dispatch loop it grew that frame.
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
        let key = frame.literal(inst.c());
        let cache = frame.runtime.caches.field(cache_slot);
        match self.resolve_index(receiver, IndexStart::Receiver, key, cache, local_cost)? {
            IndexOutcome::Value(val) => self.stack[dst] = val,
            // native-call: reg_get_field->index_call
            IndexOutcome::Call(units) => self.index_call(dst, units, call_site)?,
        }
        Ok(())
    }

    /// Call the `__index` function handler whose window the resolver pushed
    /// and take its result into `dst`, returning the resolver's units
    /// whatever the outcome.
    #[inline(never)]
    fn index_call(&mut self, dst: usize, units: u32, call_site: usize) -> Result<()> {
        self.record_call_site(call_site);
        // native-call: index_call->call_unguarded
        let result = self.call_unguarded(ArgCount::Fixed(2), RetCount::Fixed(1));
        self.in_flight.metamethod_depth -= units;
        result?;
        self.pop_into(dst);
        Ok(())
    }

    #[inline]
    fn field_cache_hit(&self, frame: &Frame, receiver: Val, cache_slot: Option<u8>) -> Option<Val> {
        let cache = frame.runtime.caches.field(cache_slot)?;
        self.get_current_cached_field(receiver.as_object_ptr()?, cache)
    }

    /// `R[dst] = R[table][R[key]]`. A plain table, or a raw hit, answers
    /// inline; a raw miss on a table with a metatable continues past the
    /// receiver's lookup it already did, and any other receiver resolves
    /// from the start.
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
        let start = match tbl_val
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
        {
            Some(tbl) => {
                let val = tbl.get(&key_val);
                match tbl.get_metatable() {
                    Some(metatable) if matches!(val, Val::Nil) => {
                        IndexStart::AfterRawMiss(metatable)
                    }
                    _ => {
                        self.stack[dst] = val;
                        return Ok(());
                    }
                }
            }
            None => IndexStart::Receiver,
        };
        match self.resolve_index(tbl_val, start, key_val, None, local_cost)? {
            IndexOutcome::Value(val) => self.stack[dst] = val,
            // native-call: reg_get_table->index_call
            IndexOutcome::Call(units) => self.index_call(dst, units, call_site)?,
        }
        Ok(())
    }

    /// `R[table].name = R[src]`. Only a set-cache hit runs in the dispatch
    /// loop; everything else is one outlined store (`State::store_field`),
    /// and only a `__newindex` function handler continues into
    /// `newindex_call`.
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
        let key = frame.literal(name);
        let cache = frame.runtime.caches.set_field(cache_slot);
        if !matches!(val, Val::Nil)
            && let Some(ptr) = receiver.as_object_ptr()
            && let Some(cache) = cache
            && self.try_set_field_cached(ptr, key, val, cache)
        {
            return Ok(());
        }
        if let Some(units) = self.store_field(receiver, key, val, cache, local_cost)? {
            // native-call: reg_set_field->newindex_call
            self.newindex_call(units, call_site)?;
        }
        Ok(())
    }

    /// Call the `__newindex` function handler whose window the resolver
    /// pushed, returning the resolver's units whatever the outcome.
    #[inline(never)]
    fn newindex_call(&mut self, units: u32, call_site: usize) -> Result<()> {
        self.record_call_site(call_site);
        // native-call: newindex_call->call_unguarded
        let result = self.call_unguarded(ArgCount::Fixed(3), RetCount::Fixed(0));
        self.in_flight.metamethod_depth -= units;
        result
    }

    /// `R[table][R[key]] = R[src]`. A plain table, or an existing key, is
    /// stored inline; a key absent from a table with a metatable continues
    /// past the lookup that proved it absent.
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
        let ptr = self.stack[table]
            .as_object_ptr()
            .expect("the direct store accepted only a table");
        if let Some(units) = self.store_absent_key(ptr, key_val, val, local_cost)? {
            // native-call: reg_set_table->newindex_call
            self.newindex_call(units, call_site)?;
        }
        Ok(())
    }

    /// `State::store_new_key` outlined, for the store whose inline lookup
    /// already proved the key absent.
    #[inline(never)]
    fn store_absent_key(
        &mut self,
        ptr: super::object::ObjectPtr,
        key: Val,
        val: Val,
        local_cost: &mut u64,
    ) -> Result<Option<u32>> {
        self.store_new_key(ptr, key, val, local_cost)
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
        match self.resolve_length(val, local_cost)? {
            Some(len) => self.stack[dst] = len,
            // native-call: reg_len->len_call
            None => self.len_call(dst, call_site)?,
        }
        Ok(())
    }

    /// Call the `__len` handler whose window `State::resolve_length` pushed
    /// and take its result into `dst`.
    #[inline(never)]
    fn len_call(&mut self, dst: usize, call_site: usize) -> Result<()> {
        self.record_call_site(call_site);
        // native-call: len_call->call_unguarded
        self.call_unguarded(ArgCount::Fixed(2), RetCount::Fixed(1))?;
        self.pop_into(dst);
        Ok(())
    }

    /// `R[dst] = R[first] .. ... .. R[first + count - 1]`, read from the
    /// registers in place.
    pub(super) fn reg_concat(&mut self, dst: usize, first: usize, count: usize) -> Result<()> {
        let val = self.concat_values(first, count)?;
        self.stack[dst] = val;
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

    /// `R[dst] = closure of nested function `nested``, capturing its upvalues
    /// from the running frame. The nested function's runtime is resolved
    /// first, the one fallible step, so a refused closure has opened no
    /// upvalue; capturing and allocating cannot fail. Outlined, so the
    /// capture list and runtime handle stay out of `Frame::run`.
    #[hotpath::measure]
    #[inline(never)]
    pub(super) fn reg_closure(&mut self, frame: &Frame, dst: usize, nested: u16) -> Result<()> {
        let bytecode = frame.get_nested_bytecode(nested);
        let runtime = self.resolve_bytecode_runtime(&bytecode)?;
        let mut captured_upvalues = Vec::with_capacity(bytecode.upvalues.len());
        for desc in &bytecode.upvalues {
            let uv_ref = match desc {
                // A local of the running frame.
                UpvalueDesc::Local(idx) => {
                    self.find_or_create_upvalue(frame.stack_bottom + *idx as usize)
                }
                // Shared with the running closure.
                UpvalueDesc::Upvalue(idx) => frame.upvalues[*idx as usize],
            };
            captured_upvalues.push(uv_ref);
        }
        let obj = self.alloc_closure(bytecode, runtime, captured_upvalues);
        self.stack[dst] = Val::Obj(obj);
        Ok(())
    }

    /// Copy varargs to `R[dst..]`: exactly `n` padded with nil, or every one,
    /// leaving the top after the last for the consumer that follows.
    pub(super) fn reg_vararg(&mut self, dst: usize, count: RetCount) -> Result<()> {
        // While innermost, the activation owns the arena suffix.
        let start = self
            .in_flight
            .call_stack
            .last()
            .expect("an executing frame has an activation record")
            .vararg_start as usize;
        let available = self.in_flight.vararg_stack.len() - start;
        match count {
            RetCount::Fixed(n) => {
                let n = usize::from(n);
                let copied = n.min(available);
                if copied > 0 {
                    let varargs = &self.in_flight.vararg_stack[start..];
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
                    let varargs = &self.in_flight.vararg_stack[start..];
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
        state.in_flight.vararg_stack.extend_from_slice(&[
            Val::Num(50.0),
            Val::Num(60.0),
            Val::Num(70.0),
            Val::Num(80.0),
        ]);
        state.in_flight.call_stack.push(CallInfo {
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
            vec![crate::FaultEvent {
                site: crate::FaultSite::StackSpace,
                argument: 1,
                eligible: true,
                fired: true,
            }]
        );
    }
}
