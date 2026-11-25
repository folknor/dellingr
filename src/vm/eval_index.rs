use super::MetaName;
use super::State;
use super::Val;
use super::caches::{
    FieldLookupCacheEntry, FieldLookupCacheSlot, METHOD_ENTRIES, MethodLookupCacheEntry,
    StringMethodCacheEntry,
};
use super::object::TablePtr;

impl State {
    /// The receiver's own lookup for table `ptr`: the field cache (with its
    /// slow revalidation), then the raw lookup, which refills the cache on a
    /// hit. Returns the raw value, or the
    /// receiver's metatable after a miss.
    #[inline(always)]
    pub(super) fn get_table_field_direct(
        &self,
        ptr: TablePtr,
        key: Val,
        cache: Option<&FieldLookupCacheSlot>,
    ) -> (Option<Val>, Option<TablePtr>) {
        if let Some(val) = cache.and_then(|cache| self.get_cached_field(ptr, key, cache)) {
            return (Some(val), None);
        }

        let tbl = self.heap.table(ptr);
        if let Some((slot, val)) = tbl.find_slot(&key) {
            if let Some(cache) = cache {
                cache.set_field(FieldLookupCacheEntry { table: ptr, slot });
            }
            return (Some(val), None);
        }

        (None, tbl.get_metatable())
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
        let mt = self.heap.table(self.string_metatable?);
        let index_handler = mt.read_slot(shape)?;
        if index_handler.as_table_ptr() != Some(entry.string_lib) {
            return None;
        }
        let tbl = self.heap.table(entry.string_lib);
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
    /// value; or, for any other table receiver, the entry's position read as
    /// a key-checked hint (`Table::string_value_at`: that slot holds the
    /// site's key with a live value). `None` is not a lookup result, only
    /// "not answered here"; the caller must continue with the full lookup.
    ///
    /// The hint hit writes nothing back. Keys are unique in a table, so a
    /// live slot holding the key is the one the keyed lookup finds, and an own
    /// hit answers before any metatable is consulted. Records built in one
    /// order share positions, so a sweep over many of them hits here with no
    /// per-table shape identity: an interned shape id guarding a keyless load
    /// was proposed and refused, since a slot keeps its key beside its value,
    /// the guard would cost about what the key compare does, and it would
    /// need a Table field, interning, transitions and snapshot handling the
    /// hint does not. Measured on bygg against 1e5a451 in matched pairs:
    /// `wide_sweep` 5.1 to 2.75 s and `record_tables` 2.2 to 1.6 s (3 of 3
    /// each), `method_step` and `same_obj_read` flat. The earlier form that
    /// renewed and refilled the entry on every cross-receiver hit, with the
    /// generic `Val` comparison, made `wide_sweep` slower (5.1 to 5.9 s).
    ///
    /// A compile-time position (a `GET_FIELD_AT` carrying the constructor
    /// layout's index for a local known to hold a template table) was
    /// declined: for a site whose receivers share a layout this hint gives
    /// the same keyed-position read after one miss, so the compiler form only
    /// saves that miss, and proving which constructor a local holds needs the
    /// dataflow the `codegen.rs` header refuses. Where it could still help is
    /// a site whose receivers have competing layouts, which re-miss here
    /// because a cross-receiver hit does not refresh the entry; that is the
    /// multi-position question, not a compile-time one.
    #[inline(always)]
    pub(super) fn get_current_cached_field(
        &self,
        ptr: TablePtr,
        key: Val,
        cache: &FieldLookupCacheSlot,
    ) -> Option<Val> {
        let entry = cache.get_field()?;
        let tbl = self.heap.table(ptr);
        if entry.table == ptr {
            return tbl.read_slot(entry.slot);
        }
        tbl.string_value_at(entry.slot, key)
    }

    #[inline(always)]
    pub(super) fn get_cached_field(
        &self,
        ptr: TablePtr,
        key: Val,
        cache: &FieldLookupCacheSlot,
    ) -> Option<Val> {
        let entry = cache.get_field()?;
        // A different receiver goes to the keyed lookup: the dispatch hit
        // (`get_current_cached_field`) already tried the cached position as
        // a key-checked hint. Repeating it here behind the resolver call,
        // with a refill, was measured against fea24a2 on bygg: no wall gain, and
        // record_tables about 3 percent slower; past the call a Map probe
        // costs about what the position check and the refill do.
        if entry.table != ptr {
            return None;
        }
        let tbl = self.heap.table(ptr);
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
        receiver_metatable: TablePtr,
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

    /// The inherited-route hit alone, for a receiver with metatable
    /// `receiver_metatable` whose raw lookup has proved the key absent: the
    /// value and the number of continuing steps whose units the caller must
    /// take before answering. `None` is only "not answered here". See
    /// `RouteLookupCacheEntry` for why the epoch stands in for the route.
    #[inline(always)]
    pub(super) fn get_cached_route(
        &self,
        receiver_metatable: TablePtr,
        cache: &FieldLookupCacheSlot,
    ) -> Option<(Val, u8)> {
        let entry = cache.get_route()?;
        if entry.receiver_metatable != receiver_metatable
            || self.heap.route_epoch() != Some(entry.route_epoch)
        {
            return None;
        }
        let value = self
            .heap
            .table(entry.terminal)
            .read_slot(entry.terminal_slot)?;
        Some((value, entry.units))
    }

    /// One method entry's validation; `None` rejects this entry only.
    #[inline(always)]
    fn cached_method_at(
        &self,
        receiver_metatable: TablePtr,
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
        let metatable = self.heap.table(receiver_metatable);
        let index_handler =
            metatable.value_if_key(entry.index_field, self.meta_names.get(MetaName::Index))?;
        if index_handler != Val::table(entry.method_table) {
            return None;
        }

        // The self-index idiom (`C.__index = C`) names one table twice;
        // reuse the borrow rather than resolving the handle again.
        let method_table = if entry.method_table == receiver_metatable {
            metatable
        } else {
            self.heap.table(entry.method_table)
        };
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
