use std::str;

use super::super::compiler::{
    FieldLookupCacheEntry, FieldLookupCacheSlot, GlobalLookupCacheEntry, GlobalLookupCacheSlot,
    MethodLookupCacheEntry, StringMethodCacheEntry,
};
use super::super::error::{ErrorKind, TypeError};
use super::LuaType;
use super::Result;
use super::State;
use super::Val;
use super::frame::Frame;
use super::object::{ObjectPtr, Upvalue};
use crate::instr::{ArgCount, RetCount};

impl State {
    #[hotpath::measure]
    pub(super) fn instr_get_field(
        &mut self,
        frame: &mut Frame,
        field_id: u16,
        cache_idx: u8,
        local_cost: &mut u64,
    ) -> Result<()> {
        // Pop value, handle both tables and strings
        let val = self.pop_val();
        let key = self.get_string_constant(frame, field_id);

        let cache = frame.runtime.caches.field_lookup.get(cache_idx as usize);

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

            if let Some(result) = self.get_index_table_field_direct(val, ptr, key, cache) {
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
    /// (`State::empty`) or with a nil `__index`, indexing a string is the usual
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

        // Root the receiver and key across the interning allocation, which
        // can collect.
        self.check_stack_space(2)?;
        self.push_unchecked(receiver);
        self.push_unchecked(key);
        let index_key = self.alloc_string("__index")?;
        let key = self.pop_val();
        let receiver = self.pop_val();

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
                if let Err(e) = self.get_table_with_key_tracking_call(
                    handler_idx,
                    key,
                    local_cost,
                    handler_called,
                ) {
                    self.stack.truncate(handler_idx);
                    return Err(e);
                }
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
            state.call(ArgCount::Fixed(2), RetCount::Fixed(1))?;
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
            return tbl.get_index(entry.index).map(|(_, val)| val);
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
            return tbl.get_index(entry.index).map(|(_, val)| val);
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
        &mut self,
        receiver: Val,
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

        let index_key = self.protected_index_key(receiver, key)?;
        let receiver_table = self.heap.as_table_ref(ptr)?;
        let receiver_metatable = receiver_table.get_metatable()?;
        let metatable = self.heap.as_table_ref(receiver_metatable)?;
        let (index_field_index, index_handler) = metatable.get_with_index(&index_key)?;
        let Some(index_table) = index_handler.as_object_ptr() else {
            if let Some(cache) = cache {
                cache.set_method(MethodLookupCacheEntry {
                    receiver_metatable,
                    index_key,
                    index_field_index,
                    index_handler,
                    method_table_version: 0,
                    method_index: None,
                    globals_version: self.globals_version,
                });
            }
            return None;
        };
        let Some(method_table) = self.heap.as_table_ref(index_table) else {
            if let Some(cache) = cache {
                cache.set_method(MethodLookupCacheEntry {
                    receiver_metatable,
                    index_key,
                    index_field_index,
                    index_handler,
                    method_table_version: 0,
                    method_index: None,
                    globals_version: self.globals_version,
                });
            }
            return None;
        };
        let method_table_version = method_table.version();
        let Some((method_index, method)) = method_table.get_with_index(&key) else {
            if let Some(cache) = cache {
                cache.set_method(MethodLookupCacheEntry {
                    receiver_metatable,
                    index_key,
                    index_field_index,
                    index_handler,
                    method_table_version,
                    method_index: None,
                    globals_version: self.globals_version,
                });
            }
            return None;
        };

        if let Some(cache) = cache {
            cache.set_method(MethodLookupCacheEntry {
                receiver_metatable,
                index_key,
                index_field_index,
                index_handler,
                method_table_version,
                method_index: Some(method_index),
                globals_version: self.globals_version,
            });
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
        if entry.globals_version != self.globals_version {
            return None;
        }

        let receiver_table = self.heap.as_table_ref(ptr)?;
        if receiver_table.get_metatable() != Some(entry.receiver_metatable) {
            return None;
        }

        let metatable = self.heap.as_table_ref(entry.receiver_metatable)?;
        let (index_key, index_handler) = metatable.get_index(entry.index_field_index)?;
        if index_key != entry.index_key || index_handler != entry.index_handler {
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
            return method_table
                .get_index(method_index)
                .map(|(_, val)| Some(val));
        }

        let (cached_key, method) = method_table.get_index(method_index)?;
        if cached_key == key {
            cache.set_method(MethodLookupCacheEntry {
                receiver_metatable: entry.receiver_metatable,
                index_key: entry.index_key,
                index_field_index: entry.index_field_index,
                index_handler: entry.index_handler,
                method_table_version,
                method_index: Some(method_index),
                globals_version: self.globals_version,
            });
            Some(Some(method))
        } else {
            None
        }
    }

    #[inline(always)]
    /// Interns `"__index"` while keeping `receiver` and `key` reachable for the
    /// GC across the allocation.
    ///
    /// This pushes its two protection values *before* popping anything, so it
    /// is net-positive while it runs and must respect the cap like any other
    /// growth path. It sits on an `Option`-returning inline-cache fast path, so
    /// exhaustion is reported as `None`: the caller then falls through to the
    /// slow lookup, which performs the same work through checked pushes and
    /// surfaces a proper `StackOverflow`.
    pub(super) fn protected_index_key(&mut self, receiver: Val, key: Val) -> Option<Val> {
        if self.check_stack_space(2).is_err() {
            return None;
        }
        self.push_unchecked(receiver);
        self.push_unchecked(key);
        let index_key = self
            .alloc_string("__index")
            .expect("fixed metamethod key is below the string size limit");
        // Internal invariant, not host input: exactly the two values pushed
        // above are removed, so this uses the panicking form rather than the
        // now-fallible public `pop`.
        self.pop_val();
        self.pop_val();
        Some(index_key)
    }

    pub(super) fn instr_get_global(
        &mut self,
        frame: &Frame,
        string_num: u16,
        cache_idx: u8,
    ) -> Result<()> {
        let s = &frame.bytecode().string_literals[string_num as usize];
        let cache = frame.runtime.caches.global_lookup.get(cache_idx as usize);
        // Net-positive by one slot, matching the uncached `get_global` path.
        if let Some(val) = cache.and_then(|cache| self.get_cached_global(cache)) {
            self.check_stack_space(1)?;
            self.push_unchecked(val);
            return Ok(());
        }

        let name = str::from_utf8(s).map_err(|_| {
            self.error(ErrorKind::InternalError(
                "compiler emitted non-UTF-8 global name".to_string(),
            ))
        })?;
        let val = if let Some(slot) = crate::instr::Builtin::from_name(name) {
            self.builtins[slot as usize]
        } else if let Some(index) = self.globals.get_index_of(name) {
            if let Some(cache) = cache {
                cache.set(GlobalLookupCacheEntry {
                    globals_version: self.globals_version,
                    index,
                });
            }
            self.globals
                .get_index(index)
                .map_or_else(Val::default, |(_, val)| *val)
        } else {
            Val::Nil
        };
        self.check_stack_space(1)?;
        self.push_unchecked(val);
        Ok(())
    }

    #[inline(always)]
    pub(super) fn get_cached_global(&self, cache: &GlobalLookupCacheSlot) -> Option<Val> {
        let entry = cache.get()?;
        if entry.globals_version != self.globals_version {
            return None;
        }
        self.globals.get_index(entry.index).map(|(_, val)| *val)
    }

    /// Fast path for getting well-known builtin globals.
    #[inline(always)]
    pub(super) fn instr_get_builtin(&mut self, slot: u8) -> Result<()> {
        let val = self.builtins[slot as usize];
        self.check_stack_space(1)?;
        self.push_unchecked(val);
        Ok(())
    }

    /// Fast path for setting well-known builtin globals.
    #[inline(always)]
    pub(super) fn instr_set_builtin(&mut self, slot: u8) {
        let val = self.pop_val();
        self.builtins[slot as usize] = val;
        // Global lookup caches and conservatively guarded table-method
        // caches revalidate after a builtin binding changes. String-method
        // caches instead validate the shared string metatable's handler.
        self.globals_version = self.globals_version.wrapping_add(1);
        // Also update globals for _G compatibility, retaining existing
        // tombstones but avoiding an entry for a never-defined nil builtin.
        if let Some(builtin) = crate::instr::Builtin::from_u8(slot)
            && (!matches!(val, Val::Nil) || self.globals.contains_key(builtin.name()))
        {
            self.globals.insert(builtin.name().to_string(), val);
        }
    }

    #[inline(always)]
    pub(super) fn instr_get_local(&mut self, local_num: u8) -> Result<()> {
        let i = local_num as usize + self.stack_bottom;
        let val = self.stack[i];
        self.check_stack_space(1)?;
        self.push_unchecked(val);
        Ok(())
    }

    pub(super) fn instr_get_upvalue(&mut self, frame: &Frame, upvalue_num: u8) -> Result<()> {
        let uv_ref = frame.upvalues[upvalue_num as usize];
        let val = match self.upvalue_pool.get(uv_ref) {
            Upvalue::Open(stack_idx) => self.stack[*stack_idx],
            Upvalue::Closed(v) => *v,
        };
        self.check_stack_space(1)?;
        self.push_unchecked(val);
        Ok(())
    }

    pub(super) fn instr_set_upvalue(&mut self, frame: &Frame, upvalue_num: u8) {
        let val = self.pop_val();
        let uv_ref = frame.upvalues[upvalue_num as usize];
        match self.upvalue_pool.get(uv_ref).clone() {
            Upvalue::Open(stack_idx) => {
                self.stack[stack_idx] = val;
            }
            Upvalue::Closed(_) => {
                *self.upvalue_pool.get_mut(uv_ref) = Upvalue::Closed(val);
            }
        }
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
