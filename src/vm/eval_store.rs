use std::str;

use super::super::compiler::{
    FieldLookupCacheEntry, GlobalLookupCacheEntry, GlobalLookupCacheSlot, SetFieldLookupCacheSlot,
};
use super::super::error::{ErrorKind, TypeError};
use super::Result;
use super::State;
use super::Val;
use super::frame::Frame;
use super::object::ObjectPtr;
use crate::instr::{ArgCount, Builtin, RetCount};

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
        let key = self.get_string_constant(frame, key_id);
        let obj_ptr = self.stack[positive_offset].as_object_ptr();
        let typ = self.stack[positive_offset].typ(&self.heap);

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
    pub(super) fn instr_new_table_template(
        &mut self,
        frame: &Frame,
        template_id: u8,
    ) -> Result<()> {
        let template = frame
            .bytecode()
            .table_templates
            .get(template_id as usize)
            .ok_or_else(|| {
                self.error(ErrorKind::InternalError(format!(
                    "NewTableTemplate: invalid template {template_id}"
                )))
            })?;
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
        let key = self.get_string_constant(frame, key_id);
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
            None => Err(self.error(ErrorKind::InternalError(format!(
                "InitFieldPinned: expected table, got {typ}"
            )))),
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
            None => Err(self.error(ErrorKind::InternalError(format!(
                "InitIndex: expected table, got {tbl_typ}"
            )))),
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
                let len_handler = self.with_rooted_value(val, |state| {
                    let len_key = state
                        .alloc_string("__len")
                        .expect("a fixed metamethod name is far below MAX_STRING_BYTES");
                    state
                        .heap
                        .as_table_ref(mt_ptr)
                        .map_or(Val::Nil, |mt| mt.get(&len_key))
                });

                if !matches!(len_handler, Val::Nil) {
                    // Call __len(table). Two values pushed against the one
                    // popped above, so this is net-positive by one slot.
                    self.check_stack_space(2)?;
                    self.push_unchecked(len_handler);
                    self.push_unchecked(val);
                    Frame::flush_local_cost(self, local_cost)?;
                    self.call(ArgCount::Fixed(1), RetCount::Fixed(1))?;
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
        let key = self.get_string_constant(frame, field_id);
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
        // set_table_with_key may invoke a user `__newindex`, which is
        // host code that could in principle reorganize the stack. No
        // current path mutates `stack[idx]` mid-call (RustFn callbacks
        // operate above stack_bottom; Lua __newindex doesn't touch the
        // receiver slot), and the assert below guards against future
        // regressions. We use the captured `receiver_ptr` for cache
        // populate so the IC entry is correct even if a future change
        // weakens that invariant.
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

        let name = str::from_utf8(&frame.bytecode().string_literals[string_num as usize]).map_err(
            |_| {
                self.error(ErrorKind::InternalError(
                    "compiler emitted non-UTF-8 global name".to_string(),
                ))
            },
        )?;
        // Builtins must keep using the central setter: it bumps the version and
        // preserves the table-library fallback rebind hook. They never seed a
        // SET_GLOBAL cache slot.
        if Builtin::from_name(name).is_some() {
            self.set_global_value_owned(name.to_owned(), val);
        } else if let Some(index) = self.globals.get_index_of(name) {
            if let Some((_, target)) = self.globals.get_index_mut(index) {
                *target = val;
                if let Some(cache) = cache {
                    cache.set(GlobalLookupCacheEntry {
                        globals_version: self.globals_version,
                        index,
                    });
                }
            }
        } else {
            // Inserts deliberately remain cold: the following execution finds
            // the stable index and populates this site without allocating.
            self.set_global_value_owned(name.to_owned(), val);
        }
        Ok(())
    }

    #[inline(always)]
    fn try_set_global_cached(&mut self, cache: &GlobalLookupCacheSlot, val: Val) -> bool {
        let Some(entry) = cache.get() else {
            return false;
        };
        if entry.globals_version != self.globals_version {
            return false;
        }
        let Some((_, target)) = self.globals.get_index_mut(entry.index) else {
            return false;
        };
        *target = val;
        true
    }

    #[hotpath::measure]
    pub(super) fn instr_set_list_count(&self, count: u8) -> Result<usize> {
        if count != 0 {
            return Ok(count as usize);
        }

        let table_idx = *self.table_constructor_bases.last().ok_or_else(|| {
            self.error(ErrorKind::InternalError(
                "SetList(0): no constructor base".to_string(),
            ))
        })?;
        if table_idx < self.stack_bottom || table_idx >= self.stack.len() {
            return Err(self.error(ErrorKind::InternalError(
                "SetList(0): base out of frame".to_string(),
            )));
        }
        let is_table = self.stack[table_idx]
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
            .is_some();
        if !is_table {
            return Err(self.error(ErrorKind::InternalError(
                "SetList(0): base is not a table".to_string(),
            )));
        }
        Ok(self.stack.len() - table_idx - 1)
    }

    #[hotpath::measure]
    pub(super) fn instr_set_list(&mut self, count: u8, batch: u16) -> Result<()> {
        // Find the table on the stack (it's below the values)
        // count=0 means "use all values above the table"
        let values = if count == 0 {
            let table_idx = self.table_constructor_bases.pop().ok_or_else(|| {
                self.error(ErrorKind::InternalError(
                    "SetList(0): no constructor base".to_string(),
                ))
            })?;
            if table_idx < self.stack_bottom || table_idx >= self.stack.len() {
                return Err(self.error(ErrorKind::InternalError(
                    "SetList(0): base out of frame".to_string(),
                )));
            }
            let is_table = self.stack[table_idx]
                .as_object_ptr()
                .and_then(|ptr| self.heap.as_table_ref(ptr))
                .is_some();
            if !is_table {
                return Err(self.error(ErrorKind::InternalError(
                    "SetList(0): base is not a table".to_string(),
                )));
            }
            self.stack.split_off(table_idx + 1)
        } else {
            self.stack.split_off(self.stack.len() - count as usize)
        };
        let tbl_value = self.pop_val();
        let obj_ptr = tbl_value.as_object_ptr();
        let typ = tbl_value.typ(&self.heap);

        match obj_ptr.and_then(|ptr| self.heap.as_table(ptr)) {
            Some(tbl) => {
                let counter = usize::from(batch) * usize::from(u8::MAX) + 1..;
                for (i, val) in counter.zip(values) {
                    let key = Val::Num(i as f64);
                    tbl.insert(key, val)?;
                }
                // Net-neutral: the table was popped above and is pushed back.
                self.push_unchecked(tbl_value);
                Ok(())
            }
            None => Err(self.error(ErrorKind::InternalError(format!(
                "SetList: expected table, got {typ}"
            )))),
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
    /// `inline(always)`: this sits directly in the bytecode dispatch loop's
    /// hot arms, and an outlined call measurably regressed `arithmetic`.
    #[inline(always)]
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

    #[hotpath::measure]
    pub(super) fn get_string_constant(&self, frame: &Frame, i: u16) -> Val {
        frame.literal(i)
    }

    pub(super) fn pop_num(&mut self) -> Result<f64> {
        let val = self.pop_val();
        val.as_num()
            .ok_or_else(|| self.type_error(TypeError::Arithmetic(val.typ(&self.heap))))
    }
}
