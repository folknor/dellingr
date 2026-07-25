use std::str;

use super::super::compiler::{
    FieldLookupCacheEntry, FieldLookupCacheSlot, GlobalLookupCacheEntry, GlobalLookupCacheSlot,
    MethodLookupCacheEntry, StringMethodCacheEntry,
};
use super::super::error::{ErrorKind, TypeError};
use super::Result;
use super::State;
use super::Val;
use super::frame::Frame;
use super::object::{ObjectPtr, Upvalue};

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

        let cache = frame.caches.field_lookup.get(cache_idx as usize);

        if let Some(ptr) = val.as_object_ptr()
            && let Some((direct, has_metatable)) = self.get_table_field_direct(ptr, key, cache)
        {
            if let Some(result) = direct {
                self.stack.push(result);
                return Ok(());
            }

            if !has_metatable {
                return self.push_table_library_field(key, local_cost);
            }

            if let Some(result) = self.get_index_table_field_direct(val, ptr, key, cache) {
                self.stack.push(result);
                return Ok(());
            }

            self.stack.push(val);
            let table_idx = self.stack.len() - 1;
            self.get_table_with_key(table_idx, key, local_cost)?;
            let result = self.pop_val();
            self.pop_val();

            if matches!(result, Val::Nil) {
                self.push_table_library_field(key, local_cost)
            } else {
                self.stack.push(result);
                Ok(())
            }
        } else if val.as_string_ptr().is_some() {
            self.get_string_table_field(key, cache, local_cost)
        } else {
            Err(self.type_error(TypeError::TableIndex(val.typ(&self.heap))))
        }
    }

    /// Resolves a string index through the current global `string` table.
    pub(super) fn get_string_table_field(
        &mut self,
        key: Val,
        cache: Option<&FieldLookupCacheSlot>,
        local_cost: &mut u64,
    ) -> Result<()> {
        if let Some(cache) = cache
            && let Some(method) = self.get_cached_string_method(key, cache)
        {
            self.stack.push(method);
            return Ok(());
        }

        self.get_global("string");
        let string_lib_idx = self.stack.len() - 1;
        self.get_table_with_key(string_lib_idx, key, local_cost)?;
        let result = self.pop_val();
        let string_lib = self.pop_val();

        if let Some(cache) = cache
            && let Some(lib_ptr) = string_lib.as_object_ptr()
            && let Some(tbl) = self.heap.as_table_ref(lib_ptr)
            && let Some((index, _)) = tbl.get_with_index(&key)
        {
            cache.set_string_method(StringMethodCacheEntry {
                string_lib: lib_ptr,
                version: tbl.version(),
                index,
                globals_version: self.globals_version,
            });
        }

        self.stack.push(result);
        Ok(())
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
        // Reject the cache when the global `string` binding has been
        // rebound or swapped via with_restricted_env. Otherwise the
        // cached `string_lib` ObjectPtr (which may stay alive in
        // `saved_builtins`) silently bypasses the new binding.
        if entry.globals_version != self.globals_version {
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
                globals_version: self.globals_version,
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

        let index_key = self.protected_index_key(receiver, key);
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
    pub(super) fn protected_index_key(&mut self, receiver: Val, key: Val) -> Val {
        self.stack.push(receiver);
        self.stack.push(key);
        let index_key = self
            .alloc_string("__index")
            .expect("fixed metamethod key is below the string size limit");
        // Internal invariant, not host input: exactly the two values pushed
        // above are removed, so this uses the panicking form rather than the
        // now-fallible public `pop`.
        self.pop_val();
        self.pop_val();
        index_key
    }

    #[inline(always)]
    pub(super) fn push_table_library_field(
        &mut self,
        key: Val,
        local_cost: &mut u64,
    ) -> Result<()> {
        self.get_global("table");
        let table_lib_idx = self.stack.len() - 1;
        self.get_table_with_key(table_lib_idx, key, local_cost)?;
        let result = self.pop_val();
        self.pop_val();
        self.stack.push(result);
        Ok(())
    }

    pub(super) fn instr_get_global(
        &mut self,
        frame: &Frame,
        string_num: u16,
        cache_idx: u8,
    ) -> Result<()> {
        let s = &frame.bytecode().string_literals[string_num as usize];
        let cache = frame.caches.global_lookup.get(cache_idx as usize);
        if let Some(val) = cache.and_then(|cache| self.get_cached_global(cache)) {
            self.stack.push(val);
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
                .map(|(_, val)| *val)
                .unwrap_or_default()
        } else {
            Val::Nil
        };
        self.stack.push(val);
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
    pub(super) fn instr_get_builtin(&mut self, slot: u8) {
        let val = self.builtins[slot as usize];
        self.stack.push(val);
    }

    /// Fast path for setting well-known builtin globals.
    #[inline(always)]
    pub(super) fn instr_set_builtin(&mut self, slot: u8) {
        let val = self.pop_val();
        self.builtins[slot as usize] = val;
        // Bump globals_version so ICs holding a direct ObjectPtr into a
        // builtin library table (string-method IC, method-lookup IC
        // reaching the lib via __index) re-resolve through the new
        // binding instead of resurrecting the old one.
        self.globals_version = self.globals_version.wrapping_add(1);
        // Also update globals for _G compatibility
        if let Some(builtin) = crate::instr::Builtin::from_u8(slot) {
            self.globals.insert(builtin.name().to_string(), val);
        }
    }

    #[inline(always)]
    pub(super) fn instr_get_local(&mut self, local_num: u8) {
        let i = local_num as usize + self.stack_bottom;
        let val = self.stack[i];
        self.stack.push(val);
    }

    pub(super) fn instr_get_upvalue(&mut self, frame: &Frame, upvalue_num: u8) {
        let uv_ref = frame.upvalues[upvalue_num as usize];
        let val = match self.upvalue_pool.get(uv_ref) {
            Upvalue::Open(stack_idx) => self.stack[*stack_idx],
            Upvalue::Closed(v) => *v,
        };
        self.stack.push(val);
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
            self.get_string_table_field(key, None, local_cost)?;
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
