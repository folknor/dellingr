use super::super::error::TypeError;
use super::Result;
use super::State;
use super::Val;
use super::caches::{FieldLookupCacheEntry, SetFieldLookupCacheSlot};
use super::frame::Frame;
use super::object::ObjectPtr;
use crate::instr::{ArgCount, RetCount};

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

    /// Pop a value and push its length: a string's byte count, a table's
    /// border, or what its `__len` handler returns.
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
                    // native-call: instr_length->call_unguarded
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

    /// Pop a value and store it as `receiver.field`, the receiver sitting
    /// `stack_offset` values below it, which is removed too.
    #[hotpath::measure]
    pub(super) fn instr_set_field(
        &mut self,
        frame: &Frame,
        stack_offset: u8,
        field_id: u16,
        cache_slot: Option<u8>,
        local_cost: &mut u64,
    ) -> Result<()> {
        let val = self.pop_val();
        let idx = self.stack.len() - stack_offset as usize - 1;
        let key = frame.literal(field_id);
        let cache = frame.runtime.caches.set_field(cache_slot);

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
        // native-call: instr_set_field->set_table_with_key
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

    /// Pop a value and store it as `table[key]`, the table and key sitting
    /// `offset` values below it; both are removed too.
    #[hotpath::measure]
    pub(super) fn instr_set_table(&mut self, offset: u8, local_cost: &mut u64) -> Result<()> {
        let val = self.pop_val();
        let index = self.stack.len() - offset as usize - 2;
        let key = self.stack[index + 1];

        if !self.try_insert_table_direct(index, key, val)? {
            // native-call: instr_set_table->set_table_with_key
            self.set_table_with_key(index, key, val, local_cost)?;
        }

        self.remove_stack_pair(index);
        Ok(())
    }
}
