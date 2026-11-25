use super::super::error::TypeError;
use super::Result;
use super::State;
use super::Val;
use super::caches::{FieldLookupCacheEntry, SetFieldLookupCacheSlot};
use super::frame::Frame;
use super::object::ObjectPtr;

impl State {
    /// `R[table][key] = val` without temporaries: a plain table, or one
    /// whose metatable is known to hold no `__newindex`
    /// (`State::newindex_absent`), takes the insert, and any other table
    /// with a metatable takes an update or removal of an existing key. False
    /// leaves a key absent from such a table to `State::store_new_key`,
    /// whose `__newindex` lookup learns the absence for the next store.
    ///
    /// The absence proof is what removes the second lookup of an absent
    /// key. A hint carried from the miss into the insert was considered and
    /// declined: it saves only the receiver's probe (at most four compares
    /// inline, one hash probe in a map), still pays the metatable's lookup,
    /// and needs a second set of `insert_hash` branches keyed on whether the
    /// miss saw a dead slot. With the proof, a warm store through a class
    /// metatable pays neither lookup and reuses the plain insert.
    #[inline(always)]
    pub(super) fn try_insert_table_direct(
        &mut self,
        table_idx: usize,
        key: Val,
        val: Val,
    ) -> Result<bool> {
        let tbl_val = self.stack[table_idx];
        let ptr = tbl_val.as_object_ptr();
        match ptr.and_then(|ptr| self.heap.edit_table(ptr)) {
            Some(mut tbl) => {
                let mt = tbl.get_metatable();
                if mt.is_none() {
                    tbl.insert(key, val)?;
                    Ok(true)
                } else {
                    self.store_through_metatable(
                        ptr.expect("the receiver was just edited"),
                        mt,
                        key,
                        val,
                    )
                }
            }
            None => Err(self.type_error(TypeError::TableIndex(tbl_val.typ(&self.heap)))),
        }
    }

    /// The metatable half of [`State::try_insert_table_direct`], outlined
    /// so its borrows stay off the dispatch loop's native frame.
    #[inline(never)]
    fn store_through_metatable(
        &mut self,
        ptr: ObjectPtr,
        mt: Option<ObjectPtr>,
        key: Val,
        val: Val,
    ) -> Result<bool> {
        let absent = self.newindex_absent(mt);
        let mut tbl = self.heap.edit_table(ptr).expect("the receiver is a table");
        if absent {
            // A metatable known to hold no `__newindex` makes the store raw
            // whether or not the key is present, so the insert is the only
            // lookup.
            tbl.insert(key, val)?;
            return Ok(true);
        }
        let Some((index, _)) = tbl.get_with_index(&key) else {
            return Ok(false);
        };
        if val.is_nil() {
            Ok(tbl.remove_at_index(index))
        } else {
            // A live existing entry can be updated by its slot, so the
            // presence lookup is also the write lookup.
            Ok(tbl.set_at_index(index, val))
        }
    }

    /// `#val` up to the point where it answers or must call a handler: a
    /// string's byte count, a table's border, or, for a table whose
    /// metatable has a non-nil `__len`, `None` with cost settled and the
    /// window `[handler, val, val]` pushed for a call with two arguments and
    /// one result (`__len(t, t)`, as Lua 5.4's luaT_callTMres and the table
    /// library's `sequence_len` call it). Any non-nil handler is called:
    /// callable objects resolve through `__call`, and anything else fails
    /// in call resolution.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    #[inline(never)]
    pub(super) fn resolve_length(&mut self, val: Val, local_cost: &mut u64) -> Result<Option<Val>> {
        if let Some(s) = val.as_string(&self.heap) {
            return Ok(Some(Val::num(s.len() as f64)));
        }
        let Some(tbl) = val
            .as_object_ptr()
            .and_then(|ptr| self.heap.as_table_ref(ptr))
        else {
            return Err(self.type_error(TypeError::Length(val.typ(&self.heap))));
        };
        // The border is computed only when no handler replaces it.
        let handler = self.metatable_field(tbl.get_metatable(), super::MetaName::Len);
        if handler.is_nil() {
            return Ok(Some(Val::num(tbl.array_len() as f64)));
        }
        Frame::flush_local_cost(self, local_cost)?;
        self.push_len_window(handler, val)?;
        Ok(None)
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
        // Validate under a shared borrow: `heap.as_table` moves the global
        // table's mutation epoch, which a miss must not do.
        let (slot, renewed) = {
            let Some(tbl) = self.heap.as_table_ref(ptr) else {
                return false;
            };
            if tbl.slot_is_current(entry.slot) {
                (entry.slot, false)
            } else {
                let Some((slot, _)) = tbl.renew_slot(entry.slot, key) else {
                    return false;
                };
                (slot, true)
            }
        };
        // A live overwrite under the site's key, never `__index` (the set
        // cache is not filled for it), so inherited routes stay standing.
        let did_set = self.heap.overwrite_live_slot(ptr, slot, val);
        if did_set && renewed {
            cache.set(FieldLookupCacheEntry { table: ptr, slot });
        }
        did_set
    }
}
