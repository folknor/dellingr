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
use super::object::ObjectPtr;
use crate::instr::{ArgCount, RetCount};

/// Global names are verified UTF-8 at load time; this is not fallible host input.
pub(super) fn verified_global_name(bytes: &[u8]) -> &str {
    str::from_utf8(bytes).expect("verified global name is UTF-8")
}

impl MethodLookupCacheEntry {
    fn new(
        receiver_metatable: ObjectPtr,
        index_field_index: usize,
        index_handler: Val,
        method_table_version: u64,
        method_index: Option<usize>,
        globals_version: u64,
    ) -> Self {
        Self {
            receiver_metatable,
            index_field_index,
            index_handler,
            method_table_version,
            method_index,
            globals_version,
        }
    }
}

impl State {
    #[hotpath::measure]
    pub(super) fn instr_get_field(
        &mut self,
        frame: &mut Frame,
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
            && let Some((direct, has_metatable)) = self.get_table_field_direct(ptr, key, cache)
        {
            if let Some(result) = direct {
                self.push_unchecked(result);
                return Ok(());
            }

            if !has_metatable {
                // A miss on a plain table is nil, exactly as `t["k"]` and
                // `rawget` see it.
                self.push_unchecked(Val::Nil);
                return Ok(());
            }

            if let Some(result) = self.get_index_table_field_direct(ptr, key, cache) {
                self.push_unchecked(result);
                return Ok(());
            }

            self.push_unchecked(val);
            let table_idx = self.stack.len() - 1;
            self.get_table_with_key(table_idx, key, local_cost)?;
            let result = self.pop_val();
            self.pop_val();
            self.push_unchecked(result);
            Ok(())
        } else if val.as_string_ptr().is_some() {
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
    /// (bounded by the metamethod depth limit). Without a string metatable
    /// (`State::raw`) or with a nil `__index`, indexing a string is the usual
    /// "attempt to index a string value" error.
    ///
    /// Receiver and key are rooted here across dispatch-time interning. The
    /// final result grows the stack by one slot, and table/function dispatch
    /// needs additional temporary room; every push is preflighted here.
    pub(super) fn index_string(
        &mut self,
        receiver: Val,
        key: Val,
        cache: Option<&FieldLookupCacheSlot>,
        local_cost: &mut u64,
        handler_called: &mut bool,
    ) -> Result<()> {
        if let Some(cache) = cache
            && let Some(method) = self.get_cached_string_method(key, cache)
        {
            self.check_stack_space(1)?;
            self.push_unchecked(method);
            return Ok(());
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

        match handler {
            Val::Obj(lib_ptr) if self.heap.as_table_ref(lib_ptr).is_some() => {
                // Populate the IC from the raw slot before the lookup: when the
                // key is present raw, the lookup below returns exactly it and
                // runs no handler that could change the metatable.
                if let Some(cache) = cache
                    && let Some(shape) = shape
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
                self.check_stack_space(1)?;
                self.push_unchecked(handler);
                let handler_idx = self.stack.len() - 1;
                self.get_table_with_key_tracking_call(
                    handler_idx,
                    key,
                    local_cost,
                    handler_called,
                )?;
                let result = self.pop_val();
                self.pop_val();
                // Replaces the handler popped above: net-neutral.
                self.push_unchecked(result);
                Ok(())
            }
            Val::Obj(_) | Val::RustFn(_)
                if matches!(handler, Val::RustFn(_))
                    || handler
                        .as_object_ptr()
                        .is_some_and(|ptr| self.heap.as_lua_function(ptr).is_some()) =>
            {
                self.call_string_index_handler(handler, receiver, key, local_cost, handler_called)
            }
            Val::Str(_) => self.with_metamethod_depth(|state| {
                state.index_string(handler, key, None, local_cost, handler_called)
            }),
            Val::Nil => Err(self.type_error(TypeError::TableIndex(LuaType::String))),
            other => Err(self.type_error(TypeError::TableIndex(other.typ(&self.heap)))),
        }
    }

    /// Calls a function-valued string `__index` as `handler(receiver, key)`.
    fn call_string_index_handler(
        &mut self,
        handler: Val,
        receiver: Val,
        key: Val,
        local_cost: &mut u64,
        handler_called: &mut bool,
    ) -> Result<()> {
        self.with_metamethod_depth(|state| {
            // Settle batched cost and preflight first, so a rejection leaves
            // no temporaries behind.
            Frame::flush_local_cost(state, local_cost)?;
            state.check_stack_space(3)?;
            state.push_unchecked(handler);
            state.push_unchecked(receiver);
            state.push_unchecked(key);
            *handler_called = true;
            state.call_unguarded(ArgCount::Fixed(2), RetCount::Fixed(1))?;
            Ok(())
        })
    }

    #[inline(always)]
    pub(super) fn get_table_field_direct(
        &self,
        ptr: ObjectPtr,
        key: Val,
        cache: Option<&FieldLookupCacheSlot>,
    ) -> Option<(Option<Val>, bool)> {
        if let Some(val) = cache.and_then(|cache| self.get_cached_field(ptr, key, cache)) {
            return Some((Some(val), false));
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
            return Some((Some(val), tbl.get_metatable().is_some()));
        }

        Some((None, tbl.get_metatable().is_some()))
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
        // change (key added, removed or rehashed) misses on the version. The
        // match also proves `string_lib` is still alive, ruling out a stale
        // pointer reused by a later allocation.
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

    #[inline(always)]
    pub(super) fn get_index_table_field_direct(
        &self,
        ptr: ObjectPtr,
        key: Val,
        cache: Option<&FieldLookupCacheSlot>,
    ) -> Option<Val> {
        if cache
            .and_then(FieldLookupCacheSlot::get_method)
            .is_some_and(|entry| entry.method_index.is_none())
        {
            return None;
        }

        if let Some(cached) =
            cache.and_then(|cache| self.get_cached_index_table_field(ptr, key, cache))
        {
            return cached;
        }

        let index_key = self.meta_names.get(MetaName::Index);
        let receiver_table = self.heap.as_table_ref(ptr)?;
        let receiver_metatable = receiver_table.get_metatable()?;
        let metatable = self.heap.as_table_ref(receiver_metatable)?;
        let (index_field_index, index_handler) = metatable.get_with_index(&index_key)?;
        let Some(index_table) = index_handler.as_object_ptr() else {
            if let Some(cache) = cache {
                cache.set_method(MethodLookupCacheEntry::new(
                    receiver_metatable,
                    index_field_index,
                    index_handler,
                    0,
                    None,
                    self.global_env.version(),
                ));
            }
            return None;
        };
        let Some(method_table) = self.heap.as_table_ref(index_table) else {
            if let Some(cache) = cache {
                cache.set_method(MethodLookupCacheEntry::new(
                    receiver_metatable,
                    index_field_index,
                    index_handler,
                    0,
                    None,
                    self.global_env.version(),
                ));
            }
            return None;
        };
        let method_table_version = method_table.version();
        let Some((method_index, method)) = method_table.get_with_index(&key) else {
            if let Some(cache) = cache {
                cache.set_method(MethodLookupCacheEntry::new(
                    receiver_metatable,
                    index_field_index,
                    index_handler,
                    method_table_version,
                    None,
                    self.global_env.version(),
                ));
            }
            return None;
        };

        if let Some(cache) = cache {
            cache.set_method(MethodLookupCacheEntry::new(
                receiver_metatable,
                index_field_index,
                index_handler,
                method_table_version,
                Some(method_index),
                self.global_env.version(),
            ));
        }

        Some(method)
    }

    #[inline(always)]
    pub(super) fn get_cached_index_table_field(
        &self,
        ptr: ObjectPtr,
        key: Val,
        cache: &FieldLookupCacheSlot,
    ) -> Option<Option<Val>> {
        let entry = cache.get_method()?;

        // Reject the cache when a builtin global has been rebound or
        // sandboxed via with_restricted_env. The cached `index_handler`
        // can point at a global library table that was reachable via
        // `mt.__index = string` (or similar) and stays alive across the
        // swap, so without this check a pre-warmed callsite resurrects
        // the pre-swap binding inside the sandbox.
        if entry.globals_version != self.global_env.version() {
            return None;
        }

        let receiver_table = self.heap.as_table_ref(ptr)?;
        if receiver_table.get_metatable() != Some(entry.receiver_metatable) {
            return None;
        }

        let metatable = self.heap.as_table_ref(entry.receiver_metatable)?;
        let (index_key, index_handler) = metatable.get_index(entry.index_field_index)?;
        if index_key != self.meta_names.get(MetaName::Index) || index_handler != entry.index_handler
        {
            return None;
        }

        let Some(index_table) = entry.index_handler.as_object_ptr() else {
            return Some(None);
        };
        let Some(method_table) = self.heap.as_table_ref(index_table) else {
            return Some(None);
        };
        let method_table_version = method_table.version();
        let Some(method_index) = entry.method_index else {
            return if entry.method_table_version == method_table_version {
                Some(None)
            } else {
                None
            };
        };

        if entry.method_table_version == method_table_version {
            return method_table.get_index_value(method_index).map(Some);
        }

        let (cached_key, method) = method_table.get_index(method_index)?;
        if cached_key == key {
            cache.set_method(MethodLookupCacheEntry::new(
                entry.receiver_metatable,
                entry.index_field_index,
                entry.index_handler,
                method_table_version,
                Some(method_index),
                self.global_env.version(),
            ));
            Some(Some(method))
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

        self.get_table_with_key(table_idx, key, local_cost)?;
        // Stack now: [... table, result]
        let result = self.pop_val();
        self.stack[table_idx] = result;
        Ok(())
    }
}
