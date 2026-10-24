//! State collection protocol and explicit allocation modes.

use super::{GcHeap, Markable, ObjectPtr, StringPtr, UpvalueRef};
use crate::compiler::Bytecode;
use crate::vm::inventory::{GcCleanup, GcRoot};
use crate::vm::{BytecodeRuntime, State, Val};
use std::collections::BTreeSet;
use std::sync::Arc;

/// A single allocation which cannot collect while graph edges are installed.
/// Runtime callers obtain this through `State::allocator`; snapshot graph
/// assembly explicitly borrows it from the heap. Consuming the handle ensures
/// runtime callers check the policy again for every allocation.
pub(in crate::vm) struct NoGcAllocator<'a> {
    heap: &'a mut GcHeap,
}

impl GcHeap {
    /// Begin an explicit no-collection allocation. Snapshot graph assembly and
    /// runtime-literal materialization use this while their partial results
    /// are not yet reachable from the State roots.
    pub(in crate::vm) fn no_gc_allocator(&mut self) -> NoGcAllocator<'_> {
        NoGcAllocator { heap: self }
    }
}

impl NoGcAllocator<'_> {
    pub(in crate::vm) fn alloc_lua_fn(
        self,
        bytecode: Arc<Bytecode>,
        runtime: Arc<BytecodeRuntime>,
        upvalues: Vec<UpvalueRef>,
    ) -> ObjectPtr {
        self.heap.alloc_lua_fn_raw(bytecode, runtime, upvalues)
    }

    pub(in crate::vm) fn alloc_table(self) -> ObjectPtr {
        self.heap.alloc_table_raw()
    }

    pub(in crate::vm) fn alloc_table_with_capacity(self, capacity: usize) -> ObjectPtr {
        self.heap.alloc_table_with_capacity_raw(capacity)
    }

    pub(in crate::vm) fn alloc_table_with_template(
        self,
        key_ids: &[u16],
        literals: &[Val],
    ) -> ObjectPtr {
        self.heap.alloc_table_with_template_raw(key_ids, literals)
    }

    pub(in crate::vm) fn alloc_string(self, bytes: &[u8]) -> StringPtr {
        self.heap.alloc_string_raw(bytes)
    }
}

/// Runtime-cache identities the root visit retained, for
/// `State::sweep_bytecode_caches`.
#[derive(Default)]
pub(in crate::vm) struct LiveChunks {
    /// Active chunks with their nested prototype trees, each fully
    /// traversed: `State::retain_bytecode_tree` stops at an identity already
    /// here, which is only sound because every entry's tree is in too.
    pub(in crate::vm) trees: BTreeSet<usize>,
    /// Pending closure-shell identities, retained exactly and not
    /// traversed. Kept apart from `trees` until every tree is in, or a
    /// pending identity would stop a marked closure of the same chunk from
    /// retaining its nested prototypes.
    pub(in crate::vm) pending: Vec<usize>,
}

impl State {
    /// Check the State policy exactly once before the next heap allocation.
    /// The caller's current roots must already reach everything it holds.
    pub(in crate::vm) fn allocator(&mut self) -> NoGcAllocator<'_> {
        self.maybe_collect();
        self.heap.no_gc_allocator()
    }

    /// Run automatic collection using the complete State root set.
    fn maybe_collect(&mut self) {
        if self.heap.is_full() {
            self.collect_garbage();
        }
    }

    /// Forces a full garbage collection cycle.
    /// This marks all reachable objects and frees unreachable ones.
    ///
    /// A no-op on a poisoned State: its root set no longer describes the
    /// abandoned frames, and the State is only fit to be dropped, which needs
    /// no collection.
    pub fn gc_collect(&mut self) {
        if self.poisoned {
            return;
        }
        self.collect_garbage();
    }

    /// Mark every root `include` admits (the collector admits all of them).
    /// Heap objects go through `GcHeap::mark`, which colours the object as
    /// well as queueing it; a bare worklist push would trace its children but
    /// leave it Unmarked for the sweep to free. Bytecode-cache roots go into
    /// `live_chunks` for `sweep_bytecode_caches`.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(in crate::vm) fn mark_roots(
        &self,
        worklist: &mut Vec<ObjectPtr>,
        live_chunks: &mut LiveChunks,
        mut include: impl FnMut(&GcRoot<'_>) -> bool,
    ) {
        let heap = &self.heap;
        self.for_each_gc_root(|root| {
            if !include(&root) {
                return;
            }
            match root {
                GcRoot::Stack(values)
                | GcRoot::ChunkLiterals(values)
                | GcRoot::RootedValues(values)
                | GcRoot::Varargs(values) => values.mark_reachable(heap, worklist),
                GcRoot::Globals(env) => env.mark_reachable(heap, worklist),
                GcRoot::MetaNames(names) => names.mark(heap),
                GcRoot::StringMetatable(ptr) => heap.mark(ptr, worklist),
                GcRoot::Anchors(registry) => registry.mark_reachable(heap, worklist),
                GcRoot::SuspendedEnv(env) => env.mark_reachable(heap, worklist),
                GcRoot::OpenUpvalue(uv_ref) => self.upvalue_pool.mark_slot(uv_ref),
                GcRoot::ActiveChunk(bytecode) => {
                    State::retain_bytecode_tree(&mut live_chunks.trees, bytecode);
                }
                GcRoot::PendingChunk(identity) => live_chunks.pending.push(identity),
                #[cfg(feature = "snapshot")]
                GcRoot::EnvObject(ptr) => heap.mark(ptr, worklist),
                #[cfg(feature = "snapshot")]
                GcRoot::EnvBaseline(baseline) => {
                    for (key, value) in baseline.entries.iter().chain(&baseline.hash_slots) {
                        key.mark_reachable(heap, worklist);
                        value.mark_reachable(heap, worklist);
                    }
                    if let Some(metatable) = &baseline.metatable {
                        metatable.mark_reachable(heap, worklist);
                    }
                }
            }
        });
    }

    /// The collection cycle behind [`State::gc_collect`], without the poison
    /// check: automatic collection from allocation (and so from dispatch)
    /// calls this directly. A poisoned State never reaches it that way,
    /// because its execution is abandoned and every public entry refuses.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn collect_garbage(&mut self) {
        // Anchors the host dropped stop rooting their values here.
        self.registry.drain_released();
        // Mark all roots
        let mut worklist = self.heap.take_mark_worklist();
        let mut live_chunks = LiveChunks::default();
        self.mark_roots(&mut worklist, &mut live_chunks, |_| true);
        self.heap
            .drain_mark_worklist(&mut worklist, &self.upvalue_pool);
        self.heap.restore_mark_worklist(worklist);
        self.sweep_bytecode_caches(live_chunks);
        // Sweep unmarked objects
        self.heap.collect();
        // Weak references: clear cache entries naming what the sweep freed,
        // before any allocation can reuse those slots.
        self.drop_dead_cache_entries();
        self.for_each_gc_cleanup(|cleanup| match cleanup {
            GcCleanup::WeakTable(table, heap) => table.prune(heap),
            GcCleanup::UpvaluePool(pool) => pool.sweep(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::PATTERN_CACHE_ENTRIES;
    use crate::patterns::AnchorMode;

    /// A pending identity that is also the chunk of a marked closure must not
    /// stop that closure from retaining its nested prototypes' entries.
    #[test]
    fn pending_identity_does_not_cut_a_marked_closure_tree_short() {
        let mut state = State::new();
        state
            .load(&crate::Program::compile("local function f() end f()", None).expect("compiles"))
            .expect("load");
        let Val::Obj(main) = state.stack[state.stack.len() - 1] else {
            unreachable!("load pushes a closure");
        };
        let identity = Arc::as_ptr(
            &state
                .heap
                .as_lua_function_ref(main)
                .expect("a closure")
                .bytecode,
        ) as usize;
        state.push_val(Val::Obj(main)).expect("push");
        let _main = state.anchor().expect("anchor the main closure");
        state
            .call(crate::ArgCount::Fixed(0), crate::RetCount::Fixed(0))
            .expect("run");
        state.gc_collect();
        let entries = state.bytecode_caches.len();
        assert!(entries >= 2, "the main chunk and f both have runtimes");
        state.in_flight.pending_bytecode_caches.push(identity);
        state.gc_collect();
        state.in_flight.pending_bytecode_caches.clear();
        assert_eq!(state.bytecode_caches.len(), entries);
    }

    #[test]
    fn pattern_cache_collection_keeps_live_entries_in_eviction_order() {
        let mut state = State::new();
        state.gc_disable_auto();
        for index in 0..PATTERN_CACHE_ENTRIES {
            state.push_string(format!("pattern{index}")).unwrap();
            state.cached_pattern_at(index, AnchorMode::Honour);
        }
        let keys: Vec<_> = state.patterns.keys().copied().collect();
        let bytes = state.pattern_bytes;
        state.gc_collect();
        assert_eq!(state.patterns.keys().copied().collect::<Vec<_>>(), keys);
        assert_eq!(state.pattern_bytes, bytes);
        let compilations = state.pattern_compilations();
        state.cached_pattern_at(0, AnchorMode::Honour);
        assert_eq!(state.pattern_compilations(), compilations);

        state.push_string("extra pattern").unwrap();
        state.cached_pattern_at(PATTERN_CACHE_ENTRIES, AnchorMode::Honour);
        assert!(!state.patterns.contains_key(&keys[0]));
        assert_eq!(state.patterns.get_index(0).unwrap().0, &keys[1]);
        state
            .pop(isize::try_from(PATTERN_CACHE_ENTRIES + 1).unwrap())
            .unwrap();
        state.gc_collect();
        assert!(state.patterns.is_empty());
        assert_eq!(state.pattern_bytes, 0);
    }
}
