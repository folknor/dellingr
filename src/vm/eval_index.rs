use std::str;

use super::super::error::TypeError;
use super::LuaType;
use super::MetaName;
use super::Result;
use super::State;
use super::Val;
use super::caches::{
    FieldLookupCacheEntry, FieldLookupCacheSlot, GlobalLookupCacheSlot, MethodLookupCacheEntry,
    StringMethodCacheEntry,
};
use super::frame::Frame;
use super::metamethod::{IndexStart, IndexTarget};
use super::object::ObjectPtr;
use crate::instr::{ArgCount, RetCount};

/// Global names are verified UTF-8 at load time; this is not fallible host input.
pub(super) fn verified_global_name(bytes: &[u8]) -> &str {
    str::from_utf8(bytes).expect("verified global name is UTF-8")
}

impl State {
    #[hotpath::measure]
    pub(super) fn instr_get_field(
        &mut self,
        frame: &Frame,
        field_id: u16,
        cache_slot: Option<u8>,
        local_cost: &mut u64,
    ) -> Result<()> {
        // Pop value, handle both tables and strings
        let val = self.pop_val();
        let key = frame.literal(field_id);

        let cache = frame.runtime.caches.field(cache_slot);

        // Direct results replace the receiver popped above and need no
        // preflight. Metamethod and string-library helpers check their own
        // temporary stack growth.
        if let Some(ptr) = val.as_object_ptr()
            && let Some((direct, metatable)) = self.get_table_field_direct(ptr, key, cache)
        {
            if let Some(result) = direct {
                self.push_unchecked(result);
                return Ok(());
            }

            let Some(metatable) = metatable else {
                // A miss on a plain table is nil, exactly as `t["k"]` and
                // `rawget` see it.
                self.push_unchecked(Val::Nil);
                return Ok(());
            };

            if let Some(result) = self.get_index_table_field_direct(metatable, key, cache) {
                self.push_unchecked(result);
                return Ok(());
            }

            self.push_unchecked(val);
            let table_idx = self.stack.len() - 1;
            // native-call: instr_get_field->get_table_with_key
            self.get_table_with_key(table_idx, key, local_cost)?;
            let result = self.pop_val();
            self.pop_val();
            self.push_unchecked(result);
            Ok(())
        } else if val.as_string_ptr().is_some() {
            // native-call: instr_get_field->index_string
            self.index_string(val, key, cache, local_cost, &mut false)
        } else {
            Err(self.type_error(TypeError::TableIndex(val.typ(&self.heap))))
        }
    }

    /// Resolves `receiver[key]` for a string receiver and pushes the result.
    ///
    /// Strings share one metatable (`State::string_metatable`, built by
    /// `open_libs` with `__index` = the string library table it created), so
    /// this follows reference Lua: lookup goes through that metatable's current
    /// `__index`, never through the global `string`. Rebinding or hiding the
    /// global (`string = nil`, `with_restricted_env`) leaves `s:method()`
    /// working, and a script assigning `getmetatable("").__index` redirects
    /// it. A table `__index` is indexed with its own metamethods, a function is
    /// called as `__index(receiver, key)`, and a string is indexed again
    /// (bounded by the metamethod depth limit, under the same rule as table
    /// receivers: a terminal table step is free, every other step takes a
    /// unit). Without a string metatable
    /// (`State::raw`) or with a nil `__index`, indexing a string is the usual
    /// "attempt to index a string value" error.
    ///
    /// Receiver and key are rooted here across dispatch-time interning. The
    /// final result grows the stack by one slot, which is all a table step
    /// needs; a function handler needs its three-slot call window. Every
    /// push is preflighted (`State::walk_index_tables`).
    pub(super) fn index_string(
        &mut self,
        receiver: Val,
        key: Val,
        cache: Option<&FieldLookupCacheSlot>,
        local_cost: &mut u64,
        handler_called: &mut bool,
    ) -> Result<()> {
        let Some(handler) = self.string_index_handler(key, cache)? else {
            return Ok(());
        };

        // The same walk a table receiver's handler takes. A terminal table
        // step pushes only its result, one slot, as the cache hit above
        // does: warm and cold need the same stack.
        let mut units = 0;
        let start = IndexStart::Handler(handler);
        let result = match self.walk_index_tables(receiver, start, key, local_cost, &mut units) {
            IndexTarget::Done(result) => result,
            IndexTarget::Call => {
                // native-call: index_string->call_unguarded
                self.call_unguarded((ArgCount::Fixed(2), handler_called), RetCount::Fixed(1))
            }
            IndexTarget::Str(handler) => {
                // native-call: index_string->index_string
                self.index_string(handler, key, None, local_cost, handler_called)
            }
        };
        self.metamethod_depth -= units;
        result
    }

    /// The string metatable's `__index` handler for an `index_string`
    /// lookup of `key`, or `None` when the string-method cache answered
    /// (the method is pushed). A missing metatable or handler is the usual
    /// "attempt to index a string value" error.
    ///
    /// Outlined, with the cache population, so that their temporaries stay
    /// out of `index_string`'s frame, which stays live across a handler's
    /// re-entry.
    #[inline(never)]
    fn string_index_handler(
        &mut self,
        key: Val,
        cache: Option<&FieldLookupCacheSlot>,
    ) -> Result<Option<Val>> {
        if let Some(cache) = cache
            && let Some(method) = self.get_cached_string_method(key, cache)
        {
            self.check_stack_space(1)?;
            self.push_unchecked(method);
            return Ok(None);
        }

        let Some(mt_ptr) = self.string_metatable else {
            return Err(self.type_error(TypeError::TableIndex(LuaType::String)));
        };

        let index_key = self.meta_names.get(MetaName::Index);
        let (handler, shape) = match self.heap.as_table_ref(mt_ptr) {
            Some(mt) => match mt.get_with_index(&index_key) {
                Some((slot, handler)) => (handler, Some((mt.version(), slot))),
                None => (Val::Nil, None),
            },
            None => (Val::Nil, None),
        };

        if matches!(handler, Val::Nil) {
            return Err(self.type_error(TypeError::TableIndex(LuaType::String)));
        }
        // Populate the IC from the raw slot: when the key is present raw in
        // a library table, the walk ends at that terminal step and returns
        // exactly it, running no handler that could change the metatable.
        if let Some(cache) = cache
            && let Some(shape) = shape
            && let Some(lib_ptr) = handler.as_object_ptr()
            && let Some(tbl) = self.heap.as_table_ref(lib_ptr)
            && let Some((index, _)) = tbl.get_with_index(&key)
        {
            self.string_mt_index_shape = Some(shape);
            cache.set_string_method(StringMethodCacheEntry {
                string_lib: lib_ptr,
                version: tbl.version(),
                index,
            });
        }
        Ok(Some(handler))
    }

    #[inline(always)]
    pub(super) fn get_table_field_direct(
        &self,
        ptr: ObjectPtr,
        key: Val,
        cache: Option<&FieldLookupCacheSlot>,
    ) -> Option<(Option<Val>, Option<ObjectPtr>)> {
        if let Some(val) = cache.and_then(|cache| self.get_cached_field(ptr, key, cache)) {
            return Some((Some(val), None));
        }

        let tbl = self.heap.as_table_ref(ptr)?;
        if let Some((index, val)) = tbl.get_with_index(&key) {
            if let Some(cache) = cache {
                cache.set_field(FieldLookupCacheEntry {
                    table: ptr,
                    table_version: tbl.version(),
                    index,
                });
            }
            return Some((Some(val), None));
        }

        Some((None, tbl.get_metatable()))
    }

    #[inline(always)]
    pub(super) fn get_cached_string_method(
        &self,
        key: Val,
        cache: &FieldLookupCacheSlot,
    ) -> Option<Val> {
        let entry = cache.get_string_method()?;
        // The cached library must still be the string metatable's `__index`.
        // A value write (`getmetatable("").__index = t`) does not move the
        // metatable's shape version, so compare the slot's value; a shape
        // change (key added, removed or rehashed) misses on the version. That
        // a matching `string_lib` is the cached table and not a later one in
        // its arena slot rests on collection clearing entries that name freed
        // objects (`RuntimeCaches::drop_dead_entries`), not on the generation.
        let (mt_version, slot) = self.string_mt_index_shape?;
        let mt = self.heap.as_table_ref(self.string_metatable?)?;
        if mt.version() != mt_version {
            return None;
        }
        let (_, index_handler) = mt.get_index(slot)?;
        if index_handler.as_object_ptr() != Some(entry.string_lib) {
            return None;
        }
        let tbl = self.heap.as_table_ref(entry.string_lib)?;
        let version = tbl.version();
        if entry.version == version {
            return tbl.get_index_value(entry.index);
        }
        // Slow validation: re-read the key at the cached index. If still
        // the same method name, refresh the entry's version and use it.
        let (cached_key, cached_val) = tbl.get_index(entry.index)?;
        if cached_key == key {
            cache.set_string_method(StringMethodCacheEntry {
                string_lib: entry.string_lib,
                version,
                index: entry.index,
            });
            Some(cached_val)
        } else {
            None
        }
    }

    /// The own-field cache hit alone: same table, unchanged version, live
    /// value. `None` is not a lookup result, only "not answered here"; the
    /// caller must continue with the full lookup.
    #[inline(always)]
    pub(super) fn get_current_cached_field(
        &self,
        ptr: ObjectPtr,
        cache: &FieldLookupCacheSlot,
    ) -> Option<Val> {
        let entry = cache.get_field()?;
        if entry.table != ptr {
            return None;
        }
        let tbl = self.heap.as_table_ref(ptr)?;
        if entry.table_version != tbl.version() {
            return None;
        }
        tbl.get_index_value(entry.index)
    }

    #[inline(always)]
    pub(super) fn get_cached_field(
        &self,
        ptr: ObjectPtr,
        key: Val,
        cache: &FieldLookupCacheSlot,
    ) -> Option<Val> {
        let entry = cache.get_field()?;
        if entry.table != ptr {
            return None;
        }
        let tbl = self.heap.as_table_ref(ptr)?;
        let table_version = tbl.version();
        if entry.table_version == table_version {
            return tbl.get_index_value(entry.index);
        }
        let (cached_key, cached_val) = tbl.get_index(entry.index)?;
        if cached_key == key {
            cache.set_field(FieldLookupCacheEntry {
                table: ptr,
                table_version,
                index: entry.index,
            });
            Some(cached_val)
        } else {
            None
        }
    }

    /// The direct answer for a receiver whose raw lookup has already missed,
    /// given the receiver's metatable: the key's value in the receiver's
    /// `__index` table, when that table holds it raw. Only after the raw
    /// lookup has proved the key absent may the method cache answer: a
    /// receiver's version does not move when it gains a field, so nothing
    /// cheaper proves absence there. `None` leaves every other case (no
    /// `__index`, a handler that is not a table, a miss there, chains) to the
    /// stack path. A raw hit there is a terminal `__index` step, which takes
    /// no metamethod-depth unit on the stack path either
    /// (`State::walk_index_tables`), so the two routes agree on depth; what
    /// they still differ in is stack scratch, which the stack path needs and
    /// this one does not.
    ///
    /// The method cache can only skip the lookups, never change the answer:
    /// it holds proven hits only (`MethodLookupCacheEntry`), so whether this
    /// route answers is a function of the heap alone, the same with a cold
    /// cache, after a snapshot load, or after the site saw other receivers.
    #[inline(always)]
    pub(super) fn get_index_table_field_direct(
        &self,
        receiver_metatable: ObjectPtr,
        key: Val,
        cache: Option<&FieldLookupCacheSlot>,
    ) -> Option<Val> {
        if let Some(method) = cache
            .and_then(|cache| self.get_cached_index_table_field(receiver_metatable, key, cache))
        {
            return Some(method);
        }

        let index_key = self.meta_names.get(MetaName::Index);
        let metatable = self.heap.as_table_ref(receiver_metatable)?;
        let (index_field_index, index_handler) = metatable.get_with_index(&index_key)?;
        let method_table_ptr = index_handler.as_object_ptr()?;
        let method_table = self.heap.as_table_ref(method_table_ptr)?;
        let (method_index, method) = method_table.get_with_index(&key)?;

        if let Some(cache) = cache {
            cache.set_method(MethodLookupCacheEntry {
                receiver_metatable,
                index_field_index,
                method_table: method_table_ptr,
                method_table_version: method_table.version(),
                method_index,
            });
        }

        Some(method)
    }

    /// The method-cache hit alone, for a receiver with metatable
    /// `receiver_metatable`: it is the cached metatable, whose `__index`
    /// slot still holds the cached table, which still holds the key at the
    /// cached index with a live value. `None` is only "not answered here".
    #[inline(always)]
    fn get_cached_index_table_field(
        &self,
        receiver_metatable: ObjectPtr,
        key: Val,
        cache: &FieldLookupCacheSlot,
    ) -> Option<Val> {
        let entry = cache.get_method()?;

        // This route depends only on the receiver, its live __index handler,
        // and the method table below. Rebinding a global or swapping global
        // environments cannot change those table references: __index = string
        // stores the table value, which cold lookup also continues to follow.
        if receiver_metatable != entry.receiver_metatable {
            return None;
        }
        let metatable = self.heap.as_table_ref(receiver_metatable)?;
        let (index_key, index_handler) = metatable.get_index(entry.index_field_index)?;
        if index_key != self.meta_names.get(MetaName::Index)
            || index_handler != Val::Obj(entry.method_table)
        {
            return None;
        }

        let method_table = self.heap.as_table_ref(entry.method_table)?;
        let method_table_version = method_table.version();
        if entry.method_table_version == method_table_version {
            return method_table.get_index_value(entry.method_index);
        }

        let (cached_key, method) = method_table.get_index(entry.method_index)?;
        if cached_key == key {
            cache.set_method(MethodLookupCacheEntry {
                method_table_version,
                ..entry
            });
            Some(method)
        } else {
            None
        }
    }

    #[inline(always)]
    pub(super) fn get_cached_global(&self, cache: &GlobalLookupCacheSlot) -> Option<Val> {
        self.global_env.read_cached(cache)
    }

    #[hotpath::measure]
    pub(super) fn instr_get_table(&mut self, local_cost: &mut u64) -> Result<()> {
        let key = self.pop_val();
        // Table is now on top of the stack
        let table_idx = self.stack.len() - 1;
        let tbl_val = self.stack[table_idx];
        if tbl_val.as_string_ptr().is_some() {
            // The receiver stays on the stack (rooted) until the result
            // replaces it below.
            // native-call: instr_get_table->index_string
            self.index_string(tbl_val, key, None, local_cost, &mut false)?;
            let result = self.pop_val();
            self.stack[table_idx] = result;
            return Ok(());
        }
        let obj_ptr = tbl_val.as_object_ptr();
        let (val, has_metatable) = match obj_ptr.and_then(|ptr| self.heap.as_table_ref(ptr)) {
            Some(tbl) => {
                let val = tbl.get(&key);
                (val, tbl.get_metatable().is_some())
            }
            None => {
                let typ = tbl_val.typ(&self.heap);
                self.pop_val();
                return Err(self.type_error(TypeError::TableIndex(typ)));
            }
        };

        if !has_metatable || !matches!(val, Val::Nil) {
            self.stack[table_idx] = val;
            return Ok(());
        }

        // native-call: instr_get_table->get_table_with_key
        self.get_table_with_key(table_idx, key, local_cost)?;
        // Stack now: [... table, result]
        let result = self.pop_val();
        self.stack[table_idx] = result;
        Ok(())
    }
}
