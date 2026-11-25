//! `__index` and `__newindex` resolution.
//!
//! Every lookup and store through these metamethods is split in two. A
//! resolver walks the receiver, its handler tables and string handlers in a
//! loop, reads every table once, takes the metamethod-depth units the walk
//! needs and calls nothing. It either answers (a value, or a completed store)
//! or ends at a function handler, for which it pushes the call window. Only
//! then does the caller make the call, so the resolver's frame has returned
//! before any re-entry, and the frames that stay live across the handler are
//! the caller's alone.
//!
//! No step needs stack scratch except that call window, and no code runs
//! between the steps, so nothing the walk passes over needs rooting: during
//! the call the window roots the handler's receiver, key and value, and
//! nothing reads the walk's values after the handler returns.

use super::LuaType;
use super::MetaName;
use super::Result;
use super::State;
use super::caches::{
    FieldLookupCacheEntry, FieldLookupCacheSlot, MethodLookupCacheEntry, RouteLookupCacheEntry,
    SetFieldLookupCacheSlot, StringMethodCacheEntry,
};
use super::frame::Frame;
use super::lua_val::Val;
use super::object::ObjectPtr;
use super::table::SlotRef;
use crate::error::{ErrorKind, TypeError};
use crate::vm::lua_val::ValKind;

use crate::limits::MAX_METAMETHOD_DEPTH;

/// Where an `__index` lookup starts (`State::resolve_index`).
#[derive(Clone, Copy)]
pub(super) enum IndexStart {
    /// At the receiver: a table's own lookup, or a string's shared metatable.
    Receiver,
    /// The receiver is a table whose raw lookup already missed; this is its
    /// metatable.
    AfterRawMiss(ObjectPtr),
}

/// How an `__index` lookup resolved.
pub(super) enum IndexOutcome {
    /// The value, with every unit the walk took already returned.
    Value(Val),
    /// A function handler: the window `[handler, receiver, key]` is pushed
    /// for a call with two arguments and one result, and these units stay
    /// taken until the call ends; the caller returns them.
    Call(u32),
}

/// What a string step yields.
enum StringStep {
    /// The string-method cache, or the library table it fills from, answered.
    Done(Val),
    /// The string metatable's `__index` handler, to continue with.
    Next(Val),
}

impl State {
    /// Take one unit of the metamethod depth, or refuse at the limit. The
    /// caller returns it.
    ///
    /// Every step that nests takes one: a handler table that continues to
    /// its own `__index` or `__newindex`, a function handler, or an
    /// `__index` string handler. A terminal table step, which answers or
    /// stores in the table's raw contents, takes none
    /// (`State::resolve_index`, `State::resolve_newindex`). Function
    /// handlers count even though they are calls, so a memoising `__index`
    /// that recurses through its own table stops within
    /// `MAX_METAMETHOD_DEPTH` levels; a function
    /// handler is also a call made from Rust, so the lower
    /// `MAX_NATIVE_CALL_DEPTH` refuses that recursion first. That is
    /// deliberate and matches reference Lua, whose C call limit bounds the
    /// same recursion near 200. A `__call` function handler is exempt (see
    /// `State::begin_callable_call`) because guarding it would also cap
    /// ordinary recursion through callable objects, which reference Lua does
    /// not limit this way.
    ///
    /// Units are returned on every VM error path. A Rust panic leaves the
    /// State unusable, like other interrupted VM execution paths.
    pub(super) fn take_metamethod_unit(&mut self) -> Result<()> {
        #[cfg(not(feature = "fault-injection"))]
        let injected = false;
        #[cfg(feature = "fault-injection")]
        let injected = self.inject_fault(
            crate::FaultSite::Metamethod,
            self.in_flight.metamethod_depth as usize,
            true,
        );
        if injected || self.in_flight.metamethod_depth >= MAX_METAMETHOD_DEPTH {
            return Err(self.error(ErrorKind::MetamethodDepthExceeded {
                depth: self.in_flight.metamethod_depth,
                limit: MAX_METAMETHOD_DEPTH,
            }));
        }
        self.in_flight.metamethod_depth += 1;
        Ok(())
    }

    /// The error for an `__index` receiver or handler that can be neither
    /// indexed nor called.
    #[cold]
    #[inline(never)]
    fn index_handler_error(&self, handler: Val) -> super::Error {
        self.type_error(TypeError::TableIndex(handler.typ(&self.heap)))
    }

    /// Resolve `receiver[key]` up to the point where it answers or must call
    /// a function handler. The one lookup behind every read: `t.k`, `t:k()`
    /// and `t[k]` in bytecode, host and library reads (`State::get_table`),
    /// for table and string receivers alike. `cache` is the site's field
    /// cache, which a bytecode field read passes and nothing else does.
    ///
    /// This is where the depth rule for `__index` lives. A table step is
    /// terminal when the table holds the key raw or has no `__index` of its
    /// own to continue to: it calls nothing and recurses nowhere, so it takes
    /// no unit. A table step that continues takes one, and so does a string
    /// or function handler; a handler that can be neither indexed nor called
    /// is rejected without one. The receiver's own lookup is not a step. The
    /// caches only skip lookups, never change the answer (the method cache
    /// holds proven hits only, and answers only after the receiver's raw
    /// lookup has proved the key absent, since a receiver's version does not
    /// move when it gains a field), so whether a lookup fits under the limit
    /// depends on neither spelling nor cache history.
    ///
    /// A string receiver indexes through the shared string metatable
    /// (`State::string_metatable`, built by `open_libs` with `__index` = the
    /// string library table), never through the global `string`: rebinding
    /// or hiding the global leaves `s:method()` working, and assigning
    /// `getmetatable("").__index` redirects it. Without a string metatable
    /// (`State::raw`) or with a nil `__index`, indexing a string is the usual
    /// "attempt to index a string value" error.
    ///
    /// Every unit taken is returned before this returns, except for the
    /// `Call` outcome, whose units the caller returns once the call ends.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    #[inline(never)]
    pub(super) fn resolve_index(
        &mut self,
        receiver: Val,
        start: IndexStart,
        key: Val,
        cache: Option<&FieldLookupCacheSlot>,
        local_cost: &mut u64,
    ) -> Result<IndexOutcome> {
        let mut units = 0;
        match self.walk_index(receiver, start, key, cache, local_cost, &mut units) {
            Ok(Some(val)) => {
                self.in_flight.metamethod_depth -= units;
                Ok(IndexOutcome::Value(val))
            }
            Ok(None) => Ok(IndexOutcome::Call(units)),
            Err(error) => {
                self.in_flight.metamethod_depth -= units;
                Err(error)
            }
        }
    }

    /// The body of [`State::resolve_index`]: `Some` is the answer, `None` a
    /// pushed call window. Every unit taken is counted in `units`.
    #[inline(always)]
    fn walk_index(
        &mut self,
        receiver: Val,
        start: IndexStart,
        key: Val,
        cache: Option<&FieldLookupCacheSlot>,
        local_cost: &mut u64,
        units: &mut u32,
    ) -> Result<Option<Val>> {
        let index_key = self.meta_names.get(MetaName::Index);
        // The receiver's metatable and the slot of its `__index`, while the
        // first handler table may still fill the method cache. Only that
        // first table step fills anything: a cached route through further
        // tables re-checked on every hit did not pay (see
        // `MethodLookupCacheEntry`); a deeper raw hit fills the epoch-guarded
        // route entry instead (`RouteLookupCacheEntry`), keyed by the
        // receiver's metatable held here. Neither fills the site's own-field
        // entry: that entry means a raw slot of the table it names, so
        // naming the handler table there would accelerate nothing for this
        // receiver (the dispatch hint key-checks the receiver's own slot,
        // which lacks an inherited key) and would only evict the own entry.
        // What a table-only chain needs is covered by these two entries; a
        // string receiver or a string handler on the way leaves routes out
        // and is the remaining gap, which a field-entry fill would not close.
        let mut method_fill = None;
        let mut route_fill: Option<ObjectPtr> = None;
        let (mut receiver, mut handler) = match start {
            IndexStart::AfterRawMiss(metatable) => {
                let handler = self
                    .heap
                    .as_table_ref(metatable)
                    .map_or(Val::NIL, |mt| mt.get(&index_key));
                (receiver, handler)
            }
            IndexStart::Receiver => match receiver.kind() {
                ValKind::Obj(ptr) => {
                    let Some((direct, metatable)) = self.get_table_field_direct(ptr, key, cache)
                    else {
                        return Err(self.index_handler_error(receiver));
                    };
                    if let Some(val) = direct {
                        return Ok(Some(val));
                    }
                    // A miss on a plain table is nil, exactly as `rawget`
                    // sees it.
                    let Some(metatable) = metatable else {
                        return Ok(Some(Val::NIL));
                    };
                    if let Some(cache) = cache
                        && let Some(method) =
                            self.get_cached_index_table_field(metatable, key, cache)
                    {
                        return Ok(Some(method));
                    }
                    if let Some(cache) = cache
                        && let Some((val, steps)) = self.get_cached_route(metatable, cache)
                    {
                        // bounded: a route records at most MAX_METAMETHOD_DEPTH steps
                        for _ in 0..steps {
                            self.take_metamethod_unit()?;
                            *units += 1;
                        }
                        return Ok(Some(val));
                    }
                    let found = self
                        .heap
                        .as_table_ref(metatable)
                        .and_then(|mt| mt.find_keyed(&index_key));
                    let Some((index_field, handler)) = found else {
                        return Ok(Some(Val::NIL));
                    };
                    if cache.is_some() {
                        method_fill = Some((metatable, index_field));
                        route_fill = Some(metatable);
                    }
                    (receiver, handler)
                }
                ValKind::Str(_) => match self.string_index_step(key, cache)? {
                    StringStep::Done(val) => return Ok(Some(val)),
                    StringStep::Next(handler) => (receiver, handler),
                },
                _ => return Err(self.index_handler_error(receiver)),
            },
        };
        if handler.is_nil() {
            return Ok(Some(Val::NIL));
        }
        // With `C.__index = C` the first pass resolves the metatable handle
        // again as the handler. Reusing that borrow would mean holding it
        // into the loop, which takes depth units through `&mut self`, or
        // duplicating the first step outside the loop, the shape the loop
        // replaced so that only the caller's frames stay live; one arena
        // lookup on a walk the route and method caches mostly skip does not
        // pay for either.
        loop {
            if let Some(table_ptr) = handler.as_object_ptr()
                && let Some(table) = self.heap.as_table_ref(table_ptr)
            {
                let val = match method_fill.take() {
                    Some((receiver_metatable, index_field)) => match table.find_slot(&key) {
                        Some((method_slot, method)) => {
                            if let Some(cache) = cache {
                                cache.fill_method(MethodLookupCacheEntry {
                                    receiver_metatable,
                                    index_field,
                                    method_table: table_ptr,
                                    method_slot,
                                });
                            }
                            return Ok(Some(method));
                        }
                        None => Val::NIL,
                    },
                    None => match (route_fill, cache) {
                        // A raw hit past a continuing step: a route. The
                        // terminal is not marked: the entry names it and
                        // reads its version-carrying slot live, so a write
                        // to it (a static counter on the class that holds
                        // the method) need not retire the route. A terminal
                        // that is also a link on the route was marked by the
                        // continuing step that read it as one.
                        (Some(receiver_metatable), Some(cache)) => match table.find_slot(&key) {
                            Some((terminal_slot, val)) => {
                                if let Some(route_epoch) = self.heap.route_epoch()
                                    && let Ok(units) = u8::try_from(*units)
                                {
                                    cache.set_route(RouteLookupCacheEntry {
                                        receiver_metatable,
                                        route_epoch,
                                        terminal: table_ptr,
                                        terminal_slot,
                                        units,
                                    });
                                }
                                return Ok(Some(val));
                            }
                            None => Val::NIL,
                        },
                        _ => table.get(&key),
                    },
                };
                if !val.is_nil() {
                    return Ok(Some(val));
                }
                let next_mt = table
                    .get_metatable()
                    .and_then(|mt_ptr| self.heap.as_table_ref(mt_ptr));
                let next = next_mt.map_or(Val::NIL, |mt| mt.get(&index_key));
                if next.is_nil() {
                    return Ok(Some(Val::NIL));
                }
                // A step that continues is part of any route this walk
                // fills: mark what it read (this table's miss and metatable
                // link, and that metatable's `__index`) and, on the first
                // such step, the receiver's metatable, before the fill
                // records the epoch.
                if let Some(receiver_metatable) = route_fill {
                    table.mark_on_route();
                    if let Some(mt) = next_mt {
                        mt.mark_on_route();
                    }
                    if *units == 0
                        && let Some(mt) = self.heap.as_table_ref(receiver_metatable)
                    {
                        mt.mark_on_route();
                    }
                }
                // A table step that continues holds a unit until the lookup
                // ends.
                self.take_metamethod_unit()?;
                *units += 1;
                receiver = handler;
                handler = next;
                continue;
            }
            let calls = match handler.kind() {
                // A Lua closure or a gmatch iterator.
                ValKind::Obj(ptr) => self.heap.get(ptr).typ() == LuaType::Function,
                ValKind::RustFn(_) => true,
                ValKind::Str(_) => {
                    // Routes run through tables only.
                    route_fill = None;
                    self.take_metamethod_unit()?;
                    *units += 1;
                    // A string handler is indexed as a string receiver
                    // would be, without the site's cache.
                    match self.string_index_step(key, None)? {
                        StringStep::Done(val) => return Ok(Some(val)),
                        StringStep::Next(next) => {
                            receiver = handler;
                            handler = next;
                        }
                    }
                    continue;
                }
                _ => false,
            };
            if !calls {
                return Err(self.index_handler_error(handler));
            }
            self.take_metamethod_unit()?;
            *units += 1;
            // Settle batched cost and preflight before any push, so a
            // rejection leaves no temporaries. Cost settlement runs no user
            // code. The receiver is the table or string whose metatable
            // named the handler.
            Frame::flush_local_cost(self, local_cost)?;
            self.check_stack_space(3)?;
            self.push_unchecked(handler);
            self.push_unchecked(receiver);
            self.push_unchecked(key);
            return Ok(None);
        }
    }

    /// A string receiver's step: the string-method cache, then the string
    /// metatable's `__index`. With a cache, a key the library table holds raw
    /// fills it and answers here: that is a terminal table step, which runs
    /// no handler that could change the metatable.
    #[inline(always)]
    fn string_index_step(
        &mut self,
        key: Val,
        cache: Option<&FieldLookupCacheSlot>,
    ) -> Result<StringStep> {
        if let Some(cache) = cache
            && let Some(method) = self.get_cached_string_method(key, cache)
        {
            return Ok(StringStep::Done(method));
        }
        let Some(mt_ptr) = self.string_metatable else {
            return Err(self.type_error(TypeError::TableIndex(LuaType::String)));
        };
        let index_key = self.meta_names.get(MetaName::Index);
        let (handler, shape) = match self.heap.as_table_ref(mt_ptr) {
            Some(mt) => match mt.find_slot(&index_key) {
                Some((slot, handler)) => (handler, Some(slot)),
                None => (Val::NIL, None),
            },
            None => (Val::NIL, None),
        };
        if handler.is_nil() {
            return Err(self.type_error(TypeError::TableIndex(LuaType::String)));
        }
        if let Some(cache) = cache
            && let Some(shape) = shape
            && let Some(lib_ptr) = handler.as_object_ptr()
            && let Some(tbl) = self.heap.as_table_ref(lib_ptr)
            && let Some((slot, method)) = tbl.find_slot(&key)
        {
            self.string_mt_index_shape = Some(shape);
            cache.set_string_method(StringMethodCacheEntry {
                string_lib: lib_ptr,
                slot,
            });
            return Ok(StringStep::Done(method));
        }
        Ok(StringStep::Next(handler))
    }

    /// The `__newindex` handler of table `ptr`'s metatable, or nil.
    #[inline(always)]
    fn newindex_handler(&self, ptr: ObjectPtr) -> Val {
        let Some(mt) = self
            .heap
            .as_table_ref(ptr)
            .and_then(super::table::Table::get_metatable)
            .and_then(|mt| self.heap.as_table_ref(mt))
        else {
            return Val::NIL;
        };
        if mt.known_no_newindex() {
            debug_assert!(
                mt.get(&self.meta_names.get(MetaName::NewIndex)).is_nil(),
                "a metatable proven to lack __newindex holds one"
            );
            return Val::NIL;
        }
        let handler = mt.get(&self.meta_names.get(MetaName::NewIndex));
        if handler.is_nil() {
            mt.learn_no_newindex();
        }
        handler
    }

    /// Whether a store into table `ptr` is raw whatever its key: the table
    /// has no metatable, or its metatable is known to hold no `__newindex`
    /// (learned by an earlier `newindex_handler` lookup). Such a store needs
    /// no presence lookup first; when false, presence decides whether
    /// `__newindex` is consulted. Reads one header when a metatable is set.
    #[inline(always)]
    pub(super) fn newindex_absent(&self, mt: Option<ObjectPtr>) -> bool {
        mt.is_none_or(|mt| {
            self.heap
                .as_table_ref(mt)
                .is_none_or(super::table::Table::known_no_newindex)
        })
    }

    /// Store `receiver.key = val` for a bytecode field write the set cache
    /// did not answer. A receiver with no metatable, or whose metatable is
    /// known to hold no `__newindex` (`State::newindex_absent`), takes the
    /// store raw in one insert. Otherwise one raw lookup on the receiver (an existing key
    /// is overwritten or removed there), then `State::store_new_key`'s rule.
    /// Every raw store of a value into the receiver fills the cache with the
    /// slot it wrote. `Some(units)` is a pushed `__newindex` call window, as
    /// for `State::resolve_newindex`.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    #[inline(never)]
    pub(super) fn store_field(
        &mut self,
        receiver: Val,
        key: Val,
        val: Val,
        cache: Option<&SetFieldLookupCacheSlot>,
        local_cost: &mut u64,
    ) -> Result<Option<u32>> {
        let Some((ptr, mt)) = receiver.as_object_ptr().and_then(|ptr| {
            self.heap
                .as_table_ref(ptr)
                .map(|tbl| (ptr, tbl.get_metatable()))
        }) else {
            return Err(self.index_handler_error(receiver));
        };
        // With no metatable, or one known to hold no `__newindex`, there is
        // no `__newindex` to consult, so the store needs no presence lookup.
        let raw = self.newindex_absent(mt);
        // A site storing `__index` never fills the cache, so the cached
        // store (`State::try_set_field_cached`), which leaves inherited
        // routes standing, never rewrites a route's link.
        let cache = cache.filter(|_| key != self.meta_names.get(MetaName::Index));
        // No cross-receiver store hint (the read side's key-checked position
        // applied to stores). It is sound: `renew_slot` on the receiver
        // proves a live slot under the site's key and mints the receiver's
        // token, which is what `GcHeap::overwrite_live_slot` needs, and an
        // existing key's overwrite consults no `__newindex`. It was refused
        // on cost, not soundness. In the dispatch arm's
        // `try_set_field_cached` it grew release native levels through a
        // `SET_FIELD` by 16 bytes (`newindex_field` 656 to 672). Here,
        // behind the outlined call, it is the store twin of the read-side
        // refill that measured no gain (see `State::get_cached_field`), and
        // on a raw store it trades one mutable probe for a read and a second
        // access. No workload drives canonical cross-receiver overwrites:
        // stores over record arrays run in regions and field leaves, whose
        // record slot already reuses positions. Reopen on such a workload,
        // measured.
        // A string key with a value is a raw store whose insert reports the
        // slot it wrote, so the cache is filled without a second lookup.
        let raw_string = if val.is_nil() {
            None
        } else {
            key.as_string_ptr()
        };
        let mut tbl = self
            .heap
            .edit_table(ptr)
            .expect("checked to be a table just above");
        if raw {
            if let Some(name) = raw_string {
                let slot = tbl.store_string_key(name, val);
                Self::fill_set_cache(cache, ptr, slot);
            } else {
                tbl.insert(key, val)?;
            }
            return Ok(None);
        }
        if let Some((slot, _)) = tbl.find_slot(&key) {
            if val.is_nil() {
                tbl.remove_slot(slot);
            } else {
                tbl.store_slot(slot, val);
                Self::fill_set_cache(cache, ptr, slot);
            }
            return Ok(None);
        }
        // Absent: `State::store_new_key`, inlined here so a raw insert into
        // the receiver can fill the cache with the slot it wrote. A slot
        // only ever names the receiver's own storage; a store that went
        // through a handler leaves the cache alone, since the handler may
        // have dropped every other root of the receiver.
        let handler = self.newindex_handler(ptr);
        if !handler.is_nil() {
            return self.resolve_newindex(Val::obj(ptr), handler, key, val, local_cost);
        }
        let mut tbl = self.heap.edit_table(ptr).expect("the receiver is a table");
        if let Some(name) = raw_string {
            let slot = tbl.store_string_key(name, val);
            Self::fill_set_cache(cache, ptr, slot);
        } else {
            tbl.insert(key, val)?;
        }
        Ok(None)
    }

    #[inline(always)]
    fn fill_set_cache(cache: Option<&SetFieldLookupCacheSlot>, table: ObjectPtr, slot: SlotRef) {
        if let Some(cache) = cache {
            cache.set(FieldLookupCacheEntry { table, slot });
        }
    }

    /// Store `key = val` in table `ptr`, whose raw lookup has proved the key
    /// absent: a raw insert, or through its `__newindex`
    /// (`State::resolve_newindex`).
    #[inline(always)]
    pub(super) fn store_new_key(
        &mut self,
        ptr: ObjectPtr,
        key: Val,
        val: Val,
        local_cost: &mut u64,
    ) -> Result<Option<u32>> {
        let handler = self.newindex_handler(ptr);
        if handler.is_nil() {
            self.heap
                .edit_table(ptr)
                .expect("the receiver is a table")
                .insert(key, val)?;
            return Ok(None);
        }
        self.resolve_newindex(Val::obj(ptr), handler, key, val, local_cost)
    }

    /// Resolve a store into `target`, whose raw lookup missed and whose
    /// metatable names `handler` as its `__newindex`. Handler tables are
    /// followed in a loop, each read once: one that holds the key, or has no
    /// `__newindex` of its own, takes the store raw.
    ///
    /// This is where the depth rule for `__newindex` lives, the same as
    /// `State::resolve_index`'s for reads. A terminal table step (the raw
    /// store just described) calls nothing and recurses nowhere, so it takes
    /// no unit; a table step that continues to its own `__newindex` takes
    /// one, and so does a function handler. A handler that is neither a
    /// table nor callable is rejected without one, so at the limit its type
    /// error is what reports. The receiver's own lookup is not a step.
    ///
    /// A function handler ends the walk with its unit taken, cost settled
    /// and the window `[handler, target, key, val]` pushed for a call with
    /// three arguments and no results: `Some(units)`, which the caller
    /// returns once the call ends. `None` is a completed store, every unit
    /// already returned, as on error.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    #[inline(never)]
    pub(super) fn resolve_newindex(
        &mut self,
        target: Val,
        handler: Val,
        key: Val,
        val: Val,
        local_cost: &mut u64,
    ) -> Result<Option<u32>> {
        let mut units = 0;
        let result = self.walk_newindex(target, handler, key, val, local_cost, &mut units);
        match result {
            Ok(false) => Ok(Some(units)),
            other => {
                self.in_flight.metamethod_depth -= units;
                other.map(|_| None)
            }
        }
    }

    /// The body of [`State::resolve_newindex`]: true is a completed store,
    /// false a pushed call window.
    #[inline(always)]
    fn walk_newindex(
        &mut self,
        mut target: Val,
        mut handler: Val,
        key: Val,
        val: Val,
        local_cost: &mut u64,
        units: &mut u32,
    ) -> Result<bool> {
        loop {
            let calls = match handler.kind() {
                ValKind::Obj(ptr) => {
                    if let Some(table) = self.heap.as_table_ref(ptr) {
                        if self.newindex_absent(table.get_metatable()) {
                            // A terminal table step either way, and a raw
                            // store whether or not it holds the key: the
                            // insert is the one lookup.
                            self.heap
                                .edit_table(ptr)
                                .expect("checked to be a table just above")
                                .insert(key, val)?;
                            return Ok(true);
                        }
                        let existing = self
                            .heap
                            .as_table_ref(ptr)
                            .and_then(|table| table.find_slot(&key));
                        if let Some((slot, _)) = existing {
                            // A terminal table that holds the key: write
                            // through the slot just found, unit-free.
                            let mut table = self
                                .heap
                                .edit_table(ptr)
                                .expect("checked to be a table just above");
                            if val.is_nil() {
                                table.remove_slot(slot);
                            } else {
                                table.store_slot(slot, val);
                            }
                            return Ok(true);
                        }
                        let next = self.newindex_handler(ptr);
                        if !next.is_nil() {
                            // A table step that continues holds a unit
                            // until the store ends.
                            self.take_metamethod_unit()?;
                            *units += 1;
                            target = handler;
                            handler = next;
                            continue;
                        }
                        // A terminal table step without the key, and with no
                        // `__newindex` of its own, stores raw and takes no
                        // unit.
                        self.heap
                            .edit_table(ptr)
                            .expect("checked to be a table just above")
                            .insert(key, val)?;
                        return Ok(true);
                    }
                    // A Lua closure or a gmatch iterator.
                    self.heap.get(ptr).typ() == LuaType::Function
                }
                ValKind::RustFn(_) => true,
                _ => false,
            };
            if !calls {
                return Err(self.index_handler_error(handler));
            }
            self.take_metamethod_unit()?;
            *units += 1;
            // Settle batched cost first, then preflight, so a refusal leaves
            // no temporaries behind.
            Frame::flush_local_cost(self, local_cost)?;
            self.check_stack_space(4)?;
            self.push_unchecked(handler);
            self.push_unchecked(target);
            self.push_unchecked(key);
            self.push_unchecked(val);
            return Ok(false);
        }
    }
}
