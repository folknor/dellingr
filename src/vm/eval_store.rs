use super::super::error::{ConstructorTableOperation, ErrorKind, InternalFault, TypeError};
use super::Result;
use super::State;
use super::Val;
use super::caches::{FieldLookupCacheEntry, GlobalLookupCacheSlot, SetFieldLookupCacheSlot};
use super::frame::Frame;
use super::object::ObjectPtr;
use crate::instr::{ArgCount, RetCount};
use crate::limits::MAX_TABLE_BATCH_SIZE;

impl State {
    #[inline(always)]
    pub(super) fn remove_stack_pair(&mut self, first: usize) {
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
    pub(super) fn try_insert_table_direct(
        &mut self,
        table_idx: usize,
        key: Val,
        val: Val,
    ) -> Result<bool> {
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
            None => Err(self.type_error(TypeError::TableIndex(tbl_val.typ(&self.heap)))),
        }
    }

    #[hotpath::measure]
    pub(super) fn instr_init_field(
        &mut self,
        frame: &Frame,
        negative_offset: u8,
        key_id: u16,
    ) -> Result<()> {
        let val = self.pop_val();
        let positive_offset = self.stack.len() - negative_offset as usize - 1;
        let key = frame.literal(key_id);
        let obj_ptr = self.stack[positive_offset].as_object_ptr();
        let typ = self.stack[positive_offset].typ(&self.heap);

        match obj_ptr.and_then(|ptr| self.heap.as_table(ptr)) {
            Some(tbl) => {
                tbl.insert(key, val)?;
                Ok(())
            }
            None => Err(super::Error::internal_fault(
                InternalFault::ConstructorTableMissing {
                    operation: ConstructorTableOperation::InitField,
                    actual: typ,
                },
            )),
        }
    }

    #[hotpath::measure]
    pub(super) fn instr_new_table_template(
        &mut self,
        frame: &Frame,
        template_id: u8,
    ) -> Result<()> {
        let template = &frame.bytecode().table_templates[template_id as usize];
        self.new_table_with_template(template, &frame.runtime.literals)?;
        Ok(())
    }

    #[hotpath::measure]
    pub(super) fn instr_init_field_pinned(
        &mut self,
        frame: &Frame,
        key_id: u16,
        entry_index: u8,
    ) -> Result<()> {
        let val = self.pop_val();
        let table_idx = self.stack.len() - 1;
        let key = frame.literal(key_id);
        let obj_ptr = self.stack[table_idx].as_object_ptr();
        let typ = self.stack[table_idx].typ(&self.heap);

        match obj_ptr.and_then(|ptr| self.heap.as_table(ptr)) {
            Some(tbl) => {
                let entry_index = entry_index as usize;
                if !tbl.init_at_index(entry_index, key, val) {
                    tbl.insert(key, val)?;
                }
                Ok(())
            }
            None => Err(super::Error::internal_fault(
                InternalFault::ConstructorTableMissing {
                    operation: ConstructorTableOperation::InitFieldPinned,
                    actual: typ,
                },
            )),
        }
    }

    #[hotpath::measure]
    pub(super) fn instr_init_index(&mut self, negative_offset: u8) -> Result<()> {
        let val = self.pop_val();
        let key = self.pop_val();
        let positive_offset = self.stack.len() - negative_offset as usize - 1;
        let tbl_typ = self.stack[positive_offset].typ(&self.heap);
        let obj_ptr = self.stack[positive_offset].as_object_ptr();

        match obj_ptr.and_then(|ptr| self.heap.as_table(ptr)) {
            Some(tbl) => {
                tbl.insert(key, val)?;
                Ok(())
            }
            None => Err(super::Error::internal_fault(
                InternalFault::ConstructorTableMissing {
                    operation: ConstructorTableOperation::InitIndex,
                    actual: tbl_typ,
                },
            )),
        }
    }

    #[hotpath::measure]
    pub(super) fn instr_length(&mut self, local_cost: &mut u64) -> Result<()> {
        let val = self.pop_val();

        // Check for string first
        // The operand was popped above, so pushing a single result back is
        // net-neutral and needs no preflight.
        if let Some(s) = val.as_string(&self.heap) {
            let len = s.len();
            self.push_unchecked(Val::Num(len as f64));
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
                let len_key = self.meta_names.get(super::MetaName::Len);
                let len_handler = self
                    .heap
                    .as_table_ref(mt_ptr)
                    .map_or(Val::Nil, |mt| mt.get(&len_key));

                if !matches!(len_handler, Val::Nil) {
                    // Call __len(table, table), as Lua 5.4's luaT_callTMres
                    // does and as the table library's `sequence_len` does.
                    // Three values pushed against the one popped above, so
                    // this is net-positive by two slots.
                    Frame::flush_local_cost(self, local_cost)?;
                    self.check_stack_space(3)?;
                    self.push_unchecked(len_handler);
                    self.push_unchecked(val);
                    self.push_unchecked(val);
                    self.call_unguarded(ArgCount::Fixed(2), RetCount::Fixed(1))?;
                    return Ok(());
                }
            }

            // No __len, use default array_len
            self.push_unchecked(Val::Num(len as f64));
            return Ok(());
        }

        Err(self.type_error(TypeError::Length(val.typ(&self.heap))))
    }

    #[hotpath::measure]
    pub(super) fn instr_negate(&mut self) -> Result<()> {
        let n = self.pop_num()?;
        // Net-neutral: one operand popped, one result pushed.
        self.push_unchecked(Val::Num(-n));
        Ok(())
    }

    pub(super) fn instr_not(&mut self) {
        let b = self.pop_val().truthy();
        // Net-neutral: one operand popped, one result pushed.
        self.push_unchecked(Val::Bool(!b));
    }

    #[hotpath::measure]
    pub(super) fn instr_set_field(
        &mut self,
        frame: &Frame,
        stack_offset: u8,
        field_id: u16,
        cache_idx: u8,
        local_cost: &mut u64,
    ) -> Result<()> {
        let val = self.pop_val();
        let idx = self.stack.len() - stack_offset as usize - 1;
        let key = frame.literal(field_id);
        let cache = frame
            .runtime
            .caches
            .set_field_lookup
            .get(cache_idx as usize);

        if !matches!(val, Val::Nil)
            && let Some(ptr) = self.stack[idx].as_object_ptr()
        {
            if let Some(cache) = cache
                && self.try_set_field_cached(ptr, key, val, cache)
            {
                self.stack.remove(idx);
                return Ok(());
            }
            if self.try_set_field_direct(ptr, key, val, cache) {
                self.stack.remove(idx);
                return Ok(());
            }
        }

        let tbl_val = self.stack[idx];
        let receiver_ptr = match tbl_val
            .as_object_ptr()
            .filter(|ptr| self.heap.as_table_ref(*ptr).is_some())
        {
            Some(ptr) => ptr,
            None => {
                let typ = tbl_val.typ(&self.heap);
                return Err(self.type_error(TypeError::TableIndex(typ)));
            }
        };
        self.set_table_with_key(idx, key, val, local_cost)?;
        // Calls preserve the caller's stack prefix: Lua frames start above
        // these operands, and Rust callbacks can index only their own frame.
        // Upvalue writes may change captured locals, but not this temporary
        // receiver slot. Reallocation can move the buffer, not its indices.
        debug_assert_eq!(
            self.stack[idx], tbl_val,
            "set_table_with_key must not mutate stack[idx]; SET_FIELD cache populate relies on the captured receiver pointer"
        );

        // Populate the cache after the slow path: if the key now lives
        // in the receiver (either inserted directly, or written via
        // __newindex doing rawset), the next set is a fast hit. If the
        // key still isn't on the receiver (e.g. __newindex routed the
        // value elsewhere, or val == Nil deleted), get_with_index
        // returns None and we leave the cache cold so __newindex (or the
        // delete path) keeps firing as Lua semantics require.
        if let Some(cache) = cache
            && let Some(tbl) = self.heap.as_table_ref(receiver_ptr)
            && let Some((index, _)) = tbl.get_with_index(&key)
        {
            cache.set(FieldLookupCacheEntry {
                table: receiver_ptr,
                table_version: tbl.version(),
                index,
            });
        }

        self.stack.remove(idx);
        Ok(())
    }

    #[inline(always)]
    pub(super) fn try_set_field_cached(
        &mut self,
        ptr: ObjectPtr,
        key: Val,
        val: Val,
        cache: &SetFieldLookupCacheSlot,
    ) -> bool {
        let entry = match cache.get() {
            Some(e) => e,
            None => return false,
        };
        if entry.table != ptr {
            return false;
        }
        let (target_index, current_version, needs_refresh) = {
            let Some(tbl) = self.heap.as_table_ref(ptr) else {
                return false;
            };
            let current_version = tbl.version();
            if entry.table_version == current_version {
                (entry.index, current_version, false)
            } else {
                let Some((cached_key, _)) = tbl.get_index(entry.index) else {
                    return false;
                };
                if cached_key != key {
                    return false;
                }
                (entry.index, current_version, true)
            }
        };

        let Some(tbl) = self.heap.as_table(ptr) else {
            return false;
        };
        let did_set = tbl.set_at_index(target_index, val);
        if did_set && needs_refresh {
            cache.set(FieldLookupCacheEntry {
                table: ptr,
                table_version: current_version,
                index: target_index,
            });
        }
        did_set
    }

    #[inline(always)]
    pub(super) fn try_set_field_direct(
        &mut self,
        ptr: ObjectPtr,
        key: Val,
        val: Val,
        cache: Option<&SetFieldLookupCacheSlot>,
    ) -> bool {
        let (index, table_version) = {
            let Some(tbl) = self.heap.as_table_ref(ptr) else {
                return false;
            };
            let Some((index, _)) = tbl.get_with_index(&key) else {
                return false;
            };
            (index, tbl.version())
        };

        let Some(tbl) = self.heap.as_table(ptr) else {
            return false;
        };
        if !tbl.set_at_index(index, val) {
            return false;
        }
        if let Some(cache) = cache {
            cache.set(FieldLookupCacheEntry {
                table: ptr,
                table_version,
                index,
            });
        }
        true
    }

    #[hotpath::measure]
    pub(super) fn instr_set_global(
        &mut self,
        frame: &Frame,
        string_num: u16,
        cache_idx: u8,
    ) -> Result<()> {
        let val = self.pop_val();
        let cache = cache_idx
            .checked_sub(1)
            .and_then(|idx| frame.runtime.caches.global_lookup.get(idx as usize));
        if let Some(cache) = cache
            && self.try_set_global_cached(cache, val)
        {
            return Ok(());
        }

        let name = super::eval_index::verified_global_name(
            &frame.bytecode().string_literals[string_num as usize],
        );
        self.global_env.write_named(name, val, cache);
        Ok(())
    }

    #[inline(always)]
    fn try_set_global_cached(&mut self, cache: &GlobalLookupCacheSlot, val: Val) -> bool {
        self.global_env.write_cached(cache, val)
    }

    fn constructor_base(&self, frame: &Frame) -> Result<usize> {
        let table_idx = frame
            .constructor_base(self)
            .expect("verified dynamic set-list owns a pending constructor base");
        // Verification tracks stack shape, not value types. A valid chunk can
        // replace the marked value, so the table check remains fallible.
        let is_table = self.stack[table_idx]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .is_some();
        if !is_table {
            return Err(super::Error::internal_fault(
                InternalFault::SetListBaseNotTable,
            ));
        }
        Ok(table_idx)
    }

    #[hotpath::measure]
    pub(super) fn instr_set_list_count(&self, frame: &Frame, count: u8) -> Result<usize> {
        if count != 0 {
            return Ok(count as usize);
        }

        let table_idx = self.constructor_base(frame)?;
        Ok(self.stack.len() - table_idx - 1)
    }

    #[hotpath::measure]
    pub(super) fn instr_set_list(&mut self, frame: &Frame, count: u8, batch: u16) -> Result<()> {
        // Find the table on the stack (it's below the values)
        // count=0 means "use all values above the table"
        let first_value = if count == 0 {
            let table_idx = self.constructor_base(frame)?;
            frame.pop_constructor_base(self);
            table_idx + 1
        } else {
            self.stack.len() - count as usize
        };
        let tbl_value = self.stack[first_value - 1];
        let obj_ptr = tbl_value.as_object_ptr();
        let typ = tbl_value.typ(&self.heap);

        match obj_ptr.and_then(|ptr| self.heap.as_table(ptr)) {
            Some(tbl) => {
                // The values move straight from the stack into the table,
                // which stays in its slot. Nothing in `set_list` can enter
                // the collector or a callback, so the drained values need no
                // rooting while they are off the stack.
                tbl.set_list(
                    usize::from(batch) * usize::from(MAX_TABLE_BATCH_SIZE) + 1,
                    self.stack.drain(first_value..),
                )?;
                Ok(())
            }
            None => Err(super::Error::internal_fault(
                InternalFault::ConstructorTableMissing {
                    operation: ConstructorTableOperation::SetList,
                    actual: typ,
                },
            )),
        }
    }

    #[inline(always)]
    pub(super) fn instr_set_local(&mut self, local_num: u8) {
        let val = self.pop_val();
        let i = local_num as usize + self.stack_bottom;
        self.stack[i] = val;
    }

    #[hotpath::measure]
    pub(super) fn instr_set_table(&mut self, offset: u8, local_cost: &mut u64) -> Result<()> {
        let val = self.pop_val();
        let index = self.stack.len() - offset as usize - 2;
        let key = self.stack[index + 1];

        if !self.try_insert_table_direct(index, key, val)? {
            self.set_table_with_key(index, key, val, local_cost)?;
        }

        self.remove_stack_pair(index);
        Ok(())
    }

    // Helper methods

    /// Compare two values (numbers or strings).
    /// `target` is the ordering we're checking for.
    /// `negate` inverts the result (for <= and >=).
    #[hotpath::measure]
    pub(super) fn eval_compare(&mut self, target: std::cmp::Ordering, negate: bool) -> Result<()> {
        let result = self.eval_compare_bool(target, negate)?;
        // Net-negative: two operands popped, one result pushed.
        self.push_unchecked(Val::Bool(result));
        Ok(())
    }

    /// Pop two operands and compare them (numbers or strings), returning the
    /// result instead of pushing it. Shared by the plain comparison opcodes
    /// and the fused compare-and-branch forms; an incomparable pair (NaN
    /// involved) compares false before negation, matching the push path.
    #[inline]
    pub(super) fn eval_compare_bool(
        &mut self,
        target: std::cmp::Ordering,
        negate: bool,
    ) -> Result<bool> {
        let v2 = self.pop_val();
        let v1 = self.pop_val();

        let result = match (&v1, &v2) {
            (Val::Num(n1), Val::Num(n2)) => n1.partial_cmp(n2).map(|cmp| cmp == target),
            (Val::Str(s1), Val::Str(s2)) => {
                let cmp = self.heap.get_string(*s1).cmp(self.heap.get_string(*s2));
                Some(cmp == target)
            }
            _ => {
                // Type mismatch - error
                return Err(self.error(ErrorKind::TypeError(TypeError::Comparison(
                    v1.typ(&self.heap),
                    v2.typ(&self.heap),
                ))));
            }
        };

        Ok(result.is_some_and(|result| if negate { !result } else { result }))
    }

    #[inline(always)]
    #[hotpath::measure]
    pub(super) fn eval_float_float(&mut self, f: impl Fn(f64, f64) -> f64) -> Result<()> {
        let n2 = self.pop_num()?;
        let n1 = self.pop_num()?;
        // Net-negative: two operands popped, one result pushed.
        self.push_unchecked(Val::Num(f(n1, n2)));
        Ok(())
    }

    pub(super) fn pop_num(&mut self) -> Result<f64> {
        let val = self.pop_val();
        val.as_num()
            .ok_or_else(|| self.type_error(TypeError::Arithmetic(val.typ(&self.heap))))
    }
}
