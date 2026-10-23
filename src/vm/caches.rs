//! Per-State runtime caches for immutable bytecode.

use std::cell::Cell;

use crate::compiler::Bytecode;

use super::lua_val::Val;
use super::object::{GcHeap, ObjectPtr};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct GlobalLookupCacheEntry {
    pub(super) globals_version: u64,
    pub(super) index: GlobalLookupIndex,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GlobalLookupIndex {
    Present(usize),
    /// No binding existed at this map length. Appending a binding invalidates
    /// misses without invalidating positive slot caches. Compaction and swaps
    /// are guarded separately by `globals_version`.
    Absent(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TforCursorEntry {
    pub(super) table: ObjectPtr,
    pub(super) index: usize,
}

#[derive(Debug, Default)]
pub(super) struct TforCursorSlot {
    entry: Cell<Option<TforCursorEntry>>,
}

impl TforCursorSlot {
    pub(super) fn get(&self) -> Option<TforCursorEntry> {
        self.entry.get()
    }

    pub(super) fn set(&self, entry: TforCursorEntry) {
        self.entry.set(Some(entry));
    }
}

#[derive(Debug, Default)]
pub(super) struct GlobalLookupCacheSlot {
    entry: Cell<Option<GlobalLookupCacheEntry>>,
}

impl GlobalLookupCacheSlot {
    pub(super) fn get(&self) -> Option<GlobalLookupCacheEntry> {
        self.entry.get()
    }

    pub(super) fn set(&self, entry: GlobalLookupCacheEntry) {
        self.entry.set(Some(entry));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FieldLookupCacheEntry {
    pub(super) table: ObjectPtr,
    pub(super) table_version: u64,
    pub(super) index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct MethodLookupCacheEntry {
    pub(super) receiver_metatable: ObjectPtr,
    /// Slot of `__index` in `receiver_metatable`; the key it must still hold
    /// is the State's interned `MetaName::Index`.
    pub(super) index_field_index: usize,
    pub(super) index_handler: Val,
    pub(super) method_table_version: u64,
    pub(super) method_index: Option<usize>,
}

/// Cache for `s:method()` style calls where the receiver is a string and
/// the method is resolved through the shared string metatable's `__index`
/// table. Stores that table's identity, its version at lookup time, and the
/// method's index. The fast path also validates the live metatable handler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StringMethodCacheEntry {
    pub(super) string_lib: ObjectPtr,
    pub(super) version: u64,
    pub(super) index: usize,
}

#[derive(Debug, Default)]
pub(super) struct FieldLookupCacheSlot {
    field_entry: Cell<Option<FieldLookupCacheEntry>>,
    method_entry: Cell<Option<MethodLookupCacheEntry>>,
    string_method_entry: Cell<Option<StringMethodCacheEntry>>,
}

impl FieldLookupCacheSlot {
    pub(super) fn get_field(&self) -> Option<FieldLookupCacheEntry> {
        self.field_entry.get()
    }

    pub(super) fn set_field(&self, entry: FieldLookupCacheEntry) {
        self.field_entry.set(Some(entry));
    }

    pub(super) fn get_method(&self) -> Option<MethodLookupCacheEntry> {
        self.method_entry.get()
    }

    pub(super) fn set_method(&self, entry: MethodLookupCacheEntry) {
        self.method_entry.set(Some(entry));
    }

    pub(super) fn get_string_method(&self) -> Option<StringMethodCacheEntry> {
        self.string_method_entry.get()
    }

    pub(super) fn set_string_method(&self, entry: StringMethodCacheEntry) {
        self.string_method_entry.set(Some(entry));
    }
}

#[derive(Debug, Default)]
pub(super) struct SetFieldLookupCacheSlot {
    entry: Cell<Option<FieldLookupCacheEntry>>,
}

impl SetFieldLookupCacheSlot {
    pub(super) fn get(&self) -> Option<FieldLookupCacheEntry> {
        self.entry.get()
    }

    pub(super) fn set(&self, entry: FieldLookupCacheEntry) {
        self.entry.set(Some(entry));
    }
}

/// Per-(State, Bytecode) lookup caches owned by the State runtime bundle.
///
/// These are never shared across `State`s: cached `ObjectPtr` keys are only
/// valid inside the heap of the State that wrote them, and version cells are
/// keyed to that State's global environment version and table versions. They are never
/// serialized and are rebuilt cold when a snapshot is loaded. Their handles
/// are weak: every collection clears the entries naming what it freed
/// (`drop_dead_entries`), so no entry outlives its referent.
///
/// The interior `Cell`s give cache writes a `&self`-only borrow shape, which
/// matches the dispatch loop's invariants: simultaneous frames executing the
/// same State-local runtime bundle all see each other's writes.
#[derive(Debug, Default)]
pub(super) struct RuntimeCaches {
    pub(super) global_lookup: Vec<GlobalLookupCacheSlot>,
    pub(super) field_lookup: Vec<FieldLookupCacheSlot>,
    pub(super) set_field_lookup: Vec<SetFieldLookupCacheSlot>,
    pub(super) tfor_cursor: Vec<TforCursorSlot>,
}

// SAFETY: Every access to a cache cell must be confined to the exclusive
// execution of the one State that owns its BytecodeRuntime graph. All
// `Arc<BytecodeRuntime>` clones, including those held by frames and heap
// closures, remain inside that State and are never exposed independently.
// `State` is Send but not Sync, so moving it transfers exclusive ownership;
// safe code cannot run two executions of that State concurrently. This
// implementation would become unsound if a runtime/cache handle escaped the
// State or if separate threads could access the same cache cell at once. It is
// required for `Arc<BytecodeRuntime>` to remain Send and preserve State's Send
// guarantee.
unsafe impl Sync for RuntimeCaches {}

impl RuntimeCaches {
    /// Size every cache from the counts the Bytecode declares. Verification
    /// proves each decoded cache operand is below its family's count.
    pub(super) fn new(bc: &Bytecode) -> Self {
        fn slots<T: Default>(count: u8) -> Vec<T> {
            (0..count).map(|_| T::default()).collect()
        }
        Self {
            global_lookup: slots(bc.global_cache_slots),
            field_lookup: slots(bc.field_cache_slots),
            set_field_lookup: slots(bc.set_field_cache_slots),
            tfor_cursor: slots(bc.tfor_cursor_slots),
        }
    }

    // Lookups by a decoded cache operand (`None` is uncached). They stay
    // checked: a hand-built chunk that skipped verification falls back to the
    // uncached path instead of panicking.

    #[inline(always)]
    pub(super) fn global(&self, slot: Option<u8>) -> Option<&GlobalLookupCacheSlot> {
        self.global_lookup.get(usize::from(slot?))
    }

    #[inline(always)]
    pub(super) fn field(&self, slot: Option<u8>) -> Option<&FieldLookupCacheSlot> {
        self.field_lookup.get(usize::from(slot?))
    }

    #[inline(always)]
    pub(super) fn set_field(&self, slot: Option<u8>) -> Option<&SetFieldLookupCacheSlot> {
        self.set_field_lookup.get(usize::from(slot?))
    }

    #[inline(always)]
    pub(super) fn tfor_cursor(&self, slot: Option<u8>) -> Option<&TforCursorSlot> {
        self.tfor_cursor.get(usize::from(slot?))
    }

    /// Weak-reference processing for one runtime: clear every entry that
    /// names an object or string the collector just freed.
    ///
    /// Cache handles are weak: they are never GC roots, and a hit trusts a
    /// matching handle without re-reading the key. A generational handle
    /// alone cannot keep a dead entry from matching a later object in the
    /// same arena slot, because generations wrap. Clearing here, right after
    /// the sweep, keeps the invariant that no entry outlives its referent:
    /// entries start cold, are filled only with live handles, collection is
    /// the only thing that frees, and the sweep leaves a freed slot vacant
    /// until the next allocation, so `val_is_live` is exact at this point
    /// whatever the generation width.
    ///
    /// Every struct is destructured without `..`, so a new cache family,
    /// cell or handle field fails to compile until its verdict is decided.
    pub(super) fn drop_dead_entries(&self, heap: &GcHeap) {
        let Self {
            global_lookup,
            field_lookup,
            set_field_lookup,
            tfor_cursor,
        } = self;
        for slot in global_lookup {
            let GlobalLookupCacheSlot { entry } = slot;
            clear_dead(entry, heap);
        }
        for slot in field_lookup {
            let FieldLookupCacheSlot {
                field_entry,
                method_entry,
                string_method_entry,
            } = slot;
            clear_dead(field_entry, heap);
            clear_dead(method_entry, heap);
            clear_dead(string_method_entry, heap);
        }
        for slot in set_field_lookup {
            let SetFieldLookupCacheSlot { entry } = slot;
            clear_dead(entry, heap);
        }
        for slot in tfor_cursor {
            let TforCursorSlot { entry } = slot;
            clear_dead(entry, heap);
        }
    }

    /// Every handle any entry of this runtime names, for tests that check
    /// the weak-processing invariant.
    #[cfg(test)]
    pub(super) fn named_handles(&self) -> Vec<Val> {
        fn push<E: CacheEntry + Copy>(cell: &Cell<Option<E>>, out: &mut Vec<Val>) {
            if let Some(entry) = cell.get() {
                entry.for_each_handle(&mut |val| out.push(val));
            }
        }
        let mut out = Vec::new();
        for slot in &self.global_lookup {
            push(&slot.entry, &mut out);
        }
        for slot in &self.field_lookup {
            push(&slot.field_entry, &mut out);
            push(&slot.method_entry, &mut out);
            push(&slot.string_method_entry, &mut out);
        }
        for slot in &self.set_field_lookup {
            push(&slot.entry, &mut out);
        }
        for slot in &self.tfor_cursor {
            push(&slot.entry, &mut out);
        }
        out
    }
}

/// A cache entry's GC handles, enumerated by exhaustive destructuring.
trait CacheEntry {
    fn for_each_handle(&self, f: &mut dyn FnMut(Val));
}

impl CacheEntry for GlobalLookupCacheEntry {
    fn for_each_handle(&self, _: &mut dyn FnMut(Val)) {
        // Indexes the global environment, which is not a heap object; the
        // version is a never-reset counter that advances across swaps.
        let Self {
            globals_version: _,
            index: _,
        } = self;
    }
}

impl CacheEntry for FieldLookupCacheEntry {
    fn for_each_handle(&self, f: &mut dyn FnMut(Val)) {
        let Self {
            table,
            table_version: _,
            index: _,
        } = self;
        f(Val::Obj(*table));
    }
}

impl CacheEntry for MethodLookupCacheEntry {
    fn for_each_handle(&self, f: &mut dyn FnMut(Val)) {
        let Self {
            receiver_metatable,
            index_field_index: _,
            index_handler,
            method_table_version: _,
            method_index: _,
        } = self;
        f(Val::Obj(*receiver_metatable));
        // A table, function or string handler is a handle; other values
        // are always live.
        f(*index_handler);
    }
}

impl CacheEntry for StringMethodCacheEntry {
    fn for_each_handle(&self, f: &mut dyn FnMut(Val)) {
        let Self {
            string_lib,
            version: _,
            index: _,
        } = self;
        f(Val::Obj(*string_lib));
    }
}

impl CacheEntry for TforCursorEntry {
    fn for_each_handle(&self, f: &mut dyn FnMut(Val)) {
        // The cursor is re-validated against the control key before use, so
        // a stale one cannot give a wrong result; it is cleared anyway so the
        // invariant holds for every family without a per-family argument.
        let Self { table, index: _ } = self;
        f(Val::Obj(*table));
    }
}

fn clear_dead<E: CacheEntry + Copy>(cell: &Cell<Option<E>>, heap: &GcHeap) {
    if let Some(entry) = cell.get() {
        let mut live = true;
        entry.for_each_handle(&mut |val| live &= heap.val_is_live(&val));
        if !live {
            cell.set(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeCaches;
    use crate::compiler::compile;
    use crate::instr::Instr;

    #[test]
    fn tfor_cursor_slots_are_sequential_and_cap_at_255() {
        let source = (0..256)
            .map(|_| "for _ in pairs(t) do end")
            .collect::<Vec<_>>()
            .join("; ");
        let source = format!("local t = {{}}; {source}");
        let bc = compile(source, None).expect("source compiles");
        let calls: Vec<_> = bc
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_TFOR_CALL)
            .collect();

        assert_eq!(calls.len(), 256);
        for (index, inst) in calls.iter().take(255).enumerate() {
            assert_eq!(inst.cache_slot(), Some(index as u8));
        }
        assert_eq!(calls[255].cache_slot(), None);
        assert_eq!(bc.tfor_cursor_slots, 255);
        assert_eq!(RuntimeCaches::new(&bc).tfor_cursor.len(), 255);
    }
}
