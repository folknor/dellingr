use std::str;

use super::MetaName;
use super::State;
use super::Val;
use super::caches::{
    FieldLookupCacheEntry, FieldLookupCacheSlot, GlobalLookupCacheSlot, MethodLookupCacheEntry,
    StringMethodCacheEntry,
};
use super::object::ObjectPtr;

/// Global names are verified UTF-8 at load time; this is not fallible host input.
pub(super) fn verified_global_name(bytes: &[u8]) -> &str {
    str::from_utf8(bytes).expect("verified global name is UTF-8")
}

impl State {
    /// The receiver's own lookup for table `ptr`: the field cache (with its
    /// slow revalidation), then the raw lookup, which refills the cache on a
    /// hit. `None` if `ptr` is not a table; otherwise the raw value, or the
    /// receiver's metatable after a miss.
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

    /// The method-cache hit alone, for a receiver with metatable
    /// `receiver_metatable` whose raw lookup has proved the key absent: it is
    /// the cached metatable, whose `__index` slot still holds the cached
    /// table, which still holds the key at the cached index with a live
    /// value. `None` is only "not answered here". Entries are proven hits
    /// (`State::resolve_index` fills them from a raw hit in the first
    /// handler table, a terminal step), so the cache can skip lookups but
    /// never change an answer or its depth.
    #[inline(always)]
    pub(super) fn get_cached_index_table_field(
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
}
