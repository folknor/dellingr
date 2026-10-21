//! Per-State runtime caches for immutable bytecode.

use std::cell::Cell;

use crate::compiler::Bytecode;
use crate::instr::Instr;

use super::lua_val::Val;
use super::object::ObjectPtr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct GlobalLookupCacheEntry {
    pub(super) globals_version: u64,
    pub(super) index: usize,
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
    pub(super) index_key: Val,
    pub(super) index_field_index: usize,
    pub(super) index_handler: Val,
    pub(super) method_table_version: u64,
    pub(super) method_index: Option<usize>,
    /// Snapshot of the global environment version when the entry was populated.
    /// Bumped on builtin rebind / `with_restricted_env` swap, so a cached
    /// `index_handler` that resolved through a global library table
    /// (e.g. `__index = string`) re-validates after the binding changes.
    pub(super) globals_version: u64,
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
/// serialized and are rebuilt cold when a snapshot is loaded.
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
    pub(super) fn new(bc: &Bytecode) -> Self {
        Self {
            global_lookup: (0..bc.global_cache_slots as usize)
                .map(|_| GlobalLookupCacheSlot::default())
                .collect(),
            field_lookup: (0..bc.field_cache_slots as usize)
                .map(|_| FieldLookupCacheSlot::default())
                .collect(),
            set_field_lookup: (0..bc.set_field_cache_slots as usize)
                .map(|_| SetFieldLookupCacheSlot::default())
                .collect(),
            // Cursor slots deliberately have no Bytecode count: the three
            // existing cache counts are part of the snapshot format, and a
            // fourth would change that format. Verified C operands make this
            // high-water mark the exact allocation count.
            tfor_cursor: (0..bc
                .code
                .iter()
                .filter(|inst| inst.opcode() == Instr::OP_TFOR_CALL)
                .map(|inst| inst.c() as usize)
                .max()
                .unwrap_or(0))
                .map(|_| TforCursorSlot::default())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeCaches;
    use crate::compiler::parse_str;
    use crate::instr::Instr;

    #[test]
    fn tfor_cursor_slots_are_sequential_and_cap_at_255() {
        let source = (0..256)
            .map(|_| "for _ in pairs(t) do end")
            .collect::<Vec<_>>()
            .join("; ");
        let source = format!("local t = {{}}; {source}");
        let bc = parse_str(source).expect("source compiles");
        let calls: Vec<_> = bc
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_TFOR_CALL)
            .collect();

        assert_eq!(calls.len(), 256);
        for (index, inst) in calls.iter().take(255).enumerate() {
            assert_eq!(inst.c(), index as u8 + 1);
        }
        assert_eq!(calls[255].c(), 0);
        assert_eq!(RuntimeCaches::new(&bc).tfor_cursor.len(), 255);
    }
}
