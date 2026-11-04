use super::MetaName;
use super::State;
use super::Val;
use super::caches::{
    FieldLookupCacheEntry, FieldLookupCacheSlot, METHOD_ENTRIES, MethodLookupCacheEntry,
    StringMethodCacheEntry,
};
use super::object::ObjectPtr;

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
        if let Some((slot, val)) = tbl.find_slot(&key) {
            if let Some(cache) = cache {
                cache.set_field(FieldLookupCacheEntry { table: ptr, slot });
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
        // objects (`RuntimeCaches::drop_dead_entries`) before the slot's
        // generation can wrap (`object::CACHE_PRUNE_INTERVAL`).
        let shape = self.string_mt_index_shape?;
        let mt = self.heap.as_table_ref(self.string_metatable?)?;
        let index_handler = mt.read_slot(shape)?;
        if index_handler.as_object_ptr() != Some(entry.string_lib) {
            return None;
        }
        let tbl = self.heap.as_table_ref(entry.string_lib)?;
        if let Some(method) = tbl.slot_value(entry.slot) {
            return method;
        }
        // The library's shape moved: if its slot still holds the method
        // name, refresh the entry and use it.
        let (slot, method) = tbl.renew_slot(entry.slot, key)?;
        cache.set_string_method(StringMethodCacheEntry {
            string_lib: entry.string_lib,
            slot,
        });
        Some(method)
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
        self.heap.as_table_ref(ptr)?.read_slot(entry.slot)
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
        if let Some(val) = tbl.slot_value(entry.slot) {
            return val;
        }
        let (slot, val) = tbl.renew_slot(entry.slot, key)?;
        cache.set_field(FieldLookupCacheEntry { table: ptr, slot });
        Some(val)
    }

    /// The method-cache hit alone, for a receiver with metatable
    /// `receiver_metatable` whose raw lookup has proved the key absent: it is
    /// the cached metatable, whose `__index` slot still holds the cached
    /// table, which still holds the key at the cached index with a live
    /// value. `None` is only "not answered here". Entries are proven hits
    /// (`State::resolve_index` fills them from a raw hit in the first
    /// handler table, a terminal step), so the cache can skip lookups but
    /// never change an answer or its depth. Each entry is validated on its
    /// own; at most one can match the receiver's metatable.
    #[inline(always)]
    pub(super) fn get_cached_index_table_field(
        &self,
        receiver_metatable: ObjectPtr,
        key: Val,
        cache: &FieldLookupCacheSlot,
    ) -> Option<Val> {
        for position in 0..METHOD_ENTRIES {
            if let Some(method) = self.cached_method_at(receiver_metatable, key, cache, position) {
                cache.method_used(position);
                return Some(method);
            }
        }
        None
    }

    /// One method entry's validation; `None` rejects this entry only.
    #[inline(always)]
    fn cached_method_at(
        &self,
        receiver_metatable: ObjectPtr,
        key: Val,
        cache: &FieldLookupCacheSlot,
        position: usize,
    ) -> Option<Val> {
        let entry = cache.get_method(position)?;

        // This route depends only on the receiver, its live __index handler,
        // and the method table below. Rebinding a global or swapping global
        // environments cannot change those table references: __index = string
        // stores the table value, which cold lookup also continues to follow.
        if receiver_metatable != entry.receiver_metatable {
            return None;
        }
        let metatable = self.heap.as_table_ref(receiver_metatable)?;
        let index_handler =
            metatable.value_if_key(entry.index_field, self.meta_names.get(MetaName::Index))?;
        if index_handler != Val::Obj(entry.method_table) {
            return None;
        }

        let method_table = self.heap.as_table_ref(entry.method_table)?;
        if let Some(method) = method_table.slot_value(entry.method_slot) {
            return method;
        }
        let (method_slot, method) = method_table.renew_slot(entry.method_slot, key)?;
        cache.refresh_method(
            position,
            MethodLookupCacheEntry {
                method_slot,
                ..entry
            },
        );
        Some(method)
    }
}
