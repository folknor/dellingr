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
    FieldLookupCacheEntry, FieldLookupCacheSlot, MethodLookupCacheEntry, SetFieldLookupCacheSlot,
    StringMethodCacheEntry,
};
use super::frame::Frame;
use super::lua_val::Val;
use super::object::ObjectPtr;
use crate::error::{ErrorKind, TypeError};

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
        let injected =
            self.inject_fault("metamethod", self.in_flight.metamethod_depth as usize, true);
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
    #[hotpath::measure]
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
        // first handler table may still fill the method cache.
        let mut method_fill = None;
        let (mut receiver, mut handler) = match start {
            IndexStart::AfterRawMiss(metatable) => {
                let handler = self
                    .heap
                    .as_table_ref(metatable)
                    .map_or(Val::Nil, |mt| mt.get(&index_key));
                (receiver, handler)
            }
            IndexStart::Receiver => match receiver {
                Val::Obj(ptr) => {
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
                        return Ok(Some(Val::Nil));
                    };
                    if let Some(cache) = cache
                        && let Some(method) =
                            self.get_cached_index_table_field(metatable, key, cache)
                    {
                        return Ok(Some(method));
                    }
                    let found = self
                        .heap
                        .as_table_ref(metatable)
                        .and_then(|mt| mt.get_with_index(&index_key));
                    let Some((index_field_index, handler)) = found else {
                        return Ok(Some(Val::Nil));
                    };
                    if cache.is_some() {
                        method_fill = Some((metatable, index_field_index));
                    }
                    (receiver, handler)
                }
                Val::Str(_) => match self.string_index_step(key, cache)? {
                    StringStep::Done(val) => return Ok(Some(val)),
                    StringStep::Next(handler) => (receiver, handler),
                },
                _ => return Err(self.index_handler_error(receiver)),
            },
        };
        if matches!(handler, Val::Nil) {
            return Ok(Some(Val::Nil));
        }
        loop {
            if let Some(table_ptr) = handler.as_object_ptr()
                && let Some(table) = self.heap.as_table_ref(table_ptr)
            {
                let val = match method_fill.take() {
                    Some((receiver_metatable, index_field_index)) => {
                        match table.get_with_index(&key) {
                            Some((method_index, method)) => {
                                if let Some(cache) = cache {
                                    cache.fill_method(MethodLookupCacheEntry {
                                        receiver_metatable,
                                        index_field_index,
                                        method_table: table_ptr,
                                        method_table_version: table.version(),
                                        method_index,
                                    });
                                }
                                return Ok(Some(method));
                            }
                            None => Val::Nil,
                        }
                    }
                    None => table.get(&key),
                };
                if !matches!(val, Val::Nil) {
                    return Ok(Some(val));
                }
                let next = table
                    .get_metatable()
                    .and_then(|mt_ptr| self.heap.as_table_ref(mt_ptr))
                    .map_or(Val::Nil, |mt| mt.get(&index_key));
                if matches!(next, Val::Nil) {
                    return Ok(Some(Val::Nil));
                }
                // A table step that continues holds a unit until the lookup
                // ends.
                self.take_metamethod_unit()?;
                *units += 1;
                receiver = handler;
                handler = next;
                continue;
            }
            let calls = match handler {
                Val::Obj(ptr) => self.heap.as_lua_function_ref(ptr).is_some(),
                Val::RustFn(_) => true,
                Val::Str(_) => {
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
            Some(mt) => match mt.get_with_index(&index_key) {
                Some((slot, handler)) => (handler, Some((mt.version(), slot))),
                None => (Val::Nil, None),
            },
            None => (Val::Nil, None),
        };
        if matches!(handler, Val::Nil) {
            return Err(self.type_error(TypeError::TableIndex(LuaType::String)));
        }
        if let Some(cache) = cache
            && let Some(shape) = shape
            && let Some(lib_ptr) = handler.as_object_ptr()
            && let Some(tbl) = self.heap.as_table_ref(lib_ptr)
            && let Some((index, method)) = tbl.get_with_index(&key)
        {
            let version = tbl.version();
            self.string_mt_index_shape = Some(shape);
            cache.set_string_method(StringMethodCacheEntry {
                string_lib: lib_ptr,
                version,
                index,
            });
            return Ok(StringStep::Done(method));
        }
        Ok(StringStep::Next(handler))
    }

    /// The `__newindex` handler of table `ptr`'s metatable, or nil.
    #[inline(always)]
    fn newindex_handler(&self, ptr: ObjectPtr) -> Val {
        let newindex_key = self.meta_names.get(MetaName::NewIndex);
        self.heap
            .as_table_ref(ptr)
            .and_then(super::table::Table::get_metatable)
            .and_then(|mt| self.heap.as_table_ref(mt))
            .map_or(Val::Nil, |mt| mt.get(&newindex_key))
    }

    /// Store `receiver.key = val` for a bytecode field write the set cache
    /// did not answer: one raw lookup on the receiver (an existing key is
    /// overwritten or removed there and refills the cache), then
    /// `State::store_new_key`. `Some(units)` is a pushed `__newindex` call
    /// window, as for `State::resolve_newindex`.
    #[hotpath::measure]
    #[inline(never)]
    pub(super) fn store_field(
        &mut self,
        receiver: Val,
        key: Val,
        val: Val,
        cache: Option<&SetFieldLookupCacheSlot>,
        local_cost: &mut u64,
    ) -> Result<Option<u32>> {
        let Some(ptr) = receiver
            .as_object_ptr()
            .filter(|ptr| self.heap.as_table_ref(*ptr).is_some())
        else {
            return Err(self.index_handler_error(receiver));
        };
        let tbl = self
            .heap
            .as_table(ptr)
            .expect("checked to be a table just above");
        if let Some((index, _)) = tbl.get_with_index(&key) {
            if matches!(val, Val::Nil) {
                tbl.remove_at_index(index);
            } else {
                let table_version = tbl.version();
                tbl.set_at_index(index, val);
                if let Some(cache) = cache {
                    cache.set(FieldLookupCacheEntry {
                        table: ptr,
                        table_version,
                        index,
                    });
                }
            }
            return Ok(None);
        }
        let pending = self.store_new_key(ptr, key, val, local_cost)?;
        // A raw insert into the receiver makes the next store a cache hit.
        // After a handler nothing reads the receiver again: the handler may
        // have dropped every other root of it.
        if pending.is_none()
            && let Some(cache) = cache
            && let Some(tbl) = self.heap.as_table_ref(ptr)
            && let Some((index, _)) = tbl.get_with_index(&key)
        {
            cache.set(FieldLookupCacheEntry {
                table: ptr,
                table_version: tbl.version(),
                index,
            });
        }
        Ok(pending)
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
        if matches!(handler, Val::Nil) {
            self.heap
                .as_table(ptr)
                .expect("the receiver is a table")
                .insert(key, val)?;
            return Ok(None);
        }
        self.resolve_newindex(Val::Obj(ptr), handler, key, val, local_cost)
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
    #[hotpath::measure]
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
            let calls = match handler {
                Val::Obj(ptr) => {
                    if self.heap.as_table_ref(ptr).is_some() {
                        let existing = self
                            .heap
                            .as_table_ref(ptr)
                            .map_or(Val::Nil, |table| table.get(&key));
                        if matches!(existing, Val::Nil) {
                            let next = self.newindex_handler(ptr);
                            if !matches!(next, Val::Nil) {
                                // A table step that continues holds a unit
                                // until the store ends.
                                self.take_metamethod_unit()?;
                                *units += 1;
                                target = handler;
                                handler = next;
                                continue;
                            }
                        }
                        // A terminal table step stores raw and takes no unit.
                        self.heap
                            .as_table(ptr)
                            .expect("checked to be a table just above")
                            .insert(key, val)?;
                        return Ok(true);
                    }
                    self.heap.as_lua_function_ref(ptr).is_some()
                }
                Val::RustFn(_) => true,
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
