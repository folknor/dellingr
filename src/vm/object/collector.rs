//! State collection protocol and explicit allocation modes.

use super::{GcHeap, ObjectPtr, StringPtr, UpvalueRef};
use crate::compiler::Bytecode;
use crate::vm::{BytecodeRuntime, State, Val, mark_gc_roots};
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

    /// The collection cycle behind [`State::gc_collect`], without the poison
    /// check: automatic collection from allocation (and so from dispatch)
    /// calls this directly. A poisoned State never reaches it that way,
    /// because its execution is abandoned and every public entry refuses.
    #[hotpath::measure]
    fn collect_garbage(&mut self) {
        // This is the only production caller of the heap sweep. Clear the
        // pattern cache before freeing interned strings so no cached program
        // survives its key's lifetime. A matcher already handed out keeps its
        // own reference to the program.
        self.patterns.clear();
        self.pattern_bytes = 0;
        // Anchors the host dropped stop rooting their values here.
        self.registry.drain_released();
        // Mark all roots
        let mut worklist = self.heap.take_mark_worklist();
        mark_gc_roots(self, &mut worklist);
        self.heap
            .drain_mark_worklist(&mut worklist, &self.upvalue_pool);
        self.heap.restore_mark_worklist(worklist);
        self.sweep_bytecode_caches();
        // Sweep unmarked objects
        self.heap.collect();
        // Pool slots not marked via a reachable closure or an open-upvalue
        // entry can never be read again; recycle them.
        self.upvalue_pool.sweep();
        // `format_pointer_ids` entries are deliberately not GC roots; an entry
        // whose value no longer resolves can never be observed again (the
        // generational key cannot alias a future allocation), so drop it to
        // keep identity storage bounded by live `%p` usage.
        self.format_pointer_ids
            .retain(|key, _| self.heap.val_is_live(&key.as_val()));
    }
}
