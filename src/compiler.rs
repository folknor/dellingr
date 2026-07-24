//! Functions and types associated with converting source code into bytecode.

mod exp_desc;
mod lexer;
mod parser;
mod token;

use std::cell::Cell;
use std::sync::Arc;

use super::Instr;
use super::Result;
use super::error;
use super::vm::{ObjectPtr, Val};

/// Describes where an upvalue comes from when creating a closure.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum UpvalueDesc {
    /// Capture a local variable from the immediately enclosing function.
    Local(u8),
    /// Capture an upvalue from the immediately enclosing function.
    Upvalue(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct GlobalLookupCacheEntry {
    pub(super) globals_version: u64,
    pub(super) index: usize,
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

impl Clone for GlobalLookupCacheSlot {
    fn clone(&self) -> Self {
        // Runtime lookup caches are State-specific, so cloned chunks start cold.
        Self::default()
    }
}

impl PartialEq for GlobalLookupCacheSlot {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FieldLookupCacheEntry {
    pub(super) table: ObjectPtr,
    pub(super) table_version: u64,
    pub(super) index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MethodLookupCacheEntry {
    pub(super) receiver_metatable: ObjectPtr,
    pub(super) index_key: Val,
    pub(super) index_field_index: usize,
    pub(super) index_handler: Val,
    pub(super) method_table_version: u64,
    pub(super) method_index: Option<usize>,
    /// Snapshot of `State::globals_version` when the entry was populated.
    /// Bumped on builtin rebind / `with_restricted_env` swap, so a cached
    /// `index_handler` that resolved through a global library table
    /// (e.g. `__index = string`) re-validates after the binding changes.
    pub(super) globals_version: u64,
}

/// Cache for `s:method()` style calls where the receiver is a string and
/// the method is resolved through the `string` global library. Stores the
/// library's table identity, its version at lookup time, and the method's
/// index in that library.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StringMethodCacheEntry {
    pub(super) string_lib: ObjectPtr,
    pub(super) version: u64,
    pub(super) index: usize,
    /// Snapshot of `State::globals_version` when the entry was populated.
    /// Bumped on `string` rebind / `with_restricted_env` swap, so the
    /// cached `string_lib` ObjectPtr (which can stay reachable via
    /// `saved_builtins` even after a sandbox swap) cannot resurrect the
    /// pre-swap library through this fast path.
    pub(super) globals_version: u64,
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

impl Clone for FieldLookupCacheSlot {
    fn clone(&self) -> Self {
        // Runtime lookup caches are State-specific, so cloned chunks start cold.
        Self::default()
    }
}

impl PartialEq for FieldLookupCacheSlot {
    fn eq(&self, _other: &Self) -> bool {
        true
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

impl Clone for SetFieldLookupCacheSlot {
    fn clone(&self) -> Self {
        // Runtime lookup caches are State-specific, so cloned chunks start cold.
        Self::default()
    }
}

impl PartialEq for SetFieldLookupCacheSlot {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

/// Compiled, immutable bytecode for a single Lua function (chunk).
///
/// `Bytecode` is `Send + Sync` and `Arc`-shareable: it holds no per-execution
/// state, only the instructions, literal pools, and static metadata. The
/// per-`State` lookup caches live on `RuntimeCaches`, which is allocated
/// alongside each `Closure`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Bytecode {
    pub(crate) code: Vec<Instr>,
    pub(crate) number_literals: Vec<f64>,
    pub(crate) string_literals: Vec<Vec<u8>>,
    /// Table constructor templates. Each entry stores string-literal indices
    /// for a pure named-field constructor's keys, in insertion order.
    pub(crate) table_templates: Vec<Vec<u8>>,
    /// Number of slots in this function's global lookup cache.
    /// Cache slot indices are baked into `OP_GET_GLOBAL_CACHED` instructions.
    pub(crate) global_cache_slots: u16,
    /// Number of slots in this function's field lookup cache.
    pub(crate) field_cache_slots: u16,
    /// Number of slots in this function's set-field lookup cache.
    pub(crate) set_field_cache_slots: u8,
    pub(crate) num_params: u8,
    pub(crate) num_locals: u8,
    pub(crate) nested: Vec<Arc<Bytecode>>,
    /// Describes the upvalues this function captures.
    pub(crate) upvalues: Vec<UpvalueDesc>,
    /// Whether this function accepts varargs (...).
    pub(crate) is_vararg: bool,
    /// Optional function name (for debugging/analysis).
    pub(crate) name: Option<String>,
    /// Source name (file path or chunk identifier like "[string]").
    pub(crate) source: Option<String>,
    /// Maps instruction index to source line number.
    /// line_info[i] is the line number for code[i].
    pub(crate) line_info: Vec<u32>,
}

impl Bytecode {
    /// Walk the instruction stream, rewrite cache-able opcodes with their
    /// allocated slot index, and record per-cache slot counts on the
    /// `Bytecode`. Recurses into nested functions. Each `Closure` allocates
    /// its own `RuntimeCaches` sized from these counts.
    fn assign_cache_slots(&mut self) -> Result<()> {
        let mut global_cache_indices = vec![None; self.string_literals.len()];
        let mut global_cache_len = 0usize;
        let mut field_cache_len = 0usize;
        let mut set_field_cache_len = 0usize;

        for (pc, inst) in self.code.iter_mut().enumerate() {
            match inst.opcode() {
                Instr::OP_GET_GLOBAL => {
                    let string_idx = inst.a() as usize;
                    let Some(cache_idx) = global_cache_indices.get_mut(string_idx) else {
                        continue;
                    };
                    let cache_idx = match *cache_idx {
                        Some(cache_idx) => cache_idx,
                        None => {
                            let next_idx = u16::try_from(global_cache_len).map_err(|_| {
                                error::Error::new(
                                    error::ErrorKind::InternalError(
                                        "too many global lookup cache slots".into(),
                                    ),
                                    0,
                                    0,
                                )
                            })?;
                            *cache_idx = Some(next_idx);
                            global_cache_len += 1;
                            next_idx
                        }
                    };
                    *inst = Instr::get_global_cached(inst.a(), cache_idx);
                }
                Instr::OP_GET_FIELD => {
                    let cache_idx = u16::try_from(field_cache_len).map_err(|_| {
                        error::Error::new(
                            error::ErrorKind::InternalError(
                                "too many field lookup cache slots".into(),
                            ),
                            0,
                            0,
                        )
                    })?;
                    field_cache_len += 1;
                    *inst = Instr::get_field_cached(inst.a(), cache_idx);
                }
                Instr::OP_SET_FIELD => {
                    if set_field_cache_len == u8::MAX as usize {
                        return Err(error::Error::new(
                            error::SyntaxError::TooManyFieldAssignments,
                            self.line_info.get(pc).copied().unwrap_or(0) as usize,
                            0,
                        ));
                    }
                    let cache_idx = set_field_cache_len as u8;
                    set_field_cache_len += 1;
                    *inst = Instr::set_field_cached(inst.a(), inst.b(), cache_idx);
                }
                _ => {}
            }
        }

        self.global_cache_slots = u16::try_from(global_cache_len).map_err(|_| {
            error::Error::new(
                error::ErrorKind::InternalError("too many global lookup cache slots".into()),
                0,
                0,
            )
        })?;
        self.field_cache_slots = u16::try_from(field_cache_len).map_err(|_| {
            error::Error::new(
                error::ErrorKind::InternalError("too many field lookup cache slots".into()),
                0,
                0,
            )
        })?;
        self.set_field_cache_slots = set_field_cache_len as u8;
        Ok(())
    }
}

/// Per-execution lookup caches owned by a `Closure`.
///
/// These are never shared across `State`s: cached `ObjectPtr` keys are only
/// valid inside the heap of the State that wrote them, and version cells are
/// keyed to that State's `globals_version` / table versions. Each `Closure`
/// allocates its own caches sized from the immutable `Bytecode`.
///
/// The interior `Cell`s give cache writes a `&self`-only borrow shape, which
/// matches the dispatch loop's invariants: simultaneous frames executing the
/// same `Closure` (recursion) all see each other's writes through a shared
/// `Arc<RuntimeCaches>`.
#[derive(Debug, Default)]
pub(crate) struct RuntimeCaches {
    pub(super) global_lookup: Vec<GlobalLookupCacheSlot>,
    pub(super) field_lookup: Vec<FieldLookupCacheSlot>,
    pub(super) set_field_lookup: Vec<SetFieldLookupCacheSlot>,
}

// SAFETY: `RuntimeCaches` contains `Cell`s, which are `!Sync` in isolation.
// We claim `Sync` because every access path goes through `&mut State`, and
// `State` deliberately does not implement `Sync`. There is no way for two
// threads to simultaneously hold a `&RuntimeCaches`: cross-thread sharing of
// a `State` requires moving it (`Send`), at which point the destination
// thread holds exclusive ownership. The Cells are therefore single-threaded
// at runtime; the unsafe impl just acknowledges that the type system cannot
// see that invariant on its own.
unsafe impl Sync for RuntimeCaches {}

impl RuntimeCaches {
    pub(crate) fn new(bc: &Bytecode) -> Self {
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
        }
    }
}

#[hotpath::measure]
pub(super) fn parse_str(source: impl AsRef<str>) -> Result<Bytecode> {
    let mut bc = parser::parse_str(source.as_ref())?;
    finalize(&mut bc)?;
    Ok(bc)
}

#[hotpath::measure]
pub(super) fn parse_str_named(
    source: impl AsRef<str>,
    source_name: Option<String>,
) -> Result<Bytecode> {
    let mut bc = parser::parse_str_named(source.as_ref(), source_name)?;
    finalize(&mut bc)?;
    Ok(bc)
}

/// Walk a freshly-parsed `Bytecode` tree and assign cache slot indices to
/// every nested function before it ships to the runtime.
fn finalize(bc: &mut Bytecode) -> Result<()> {
    bc.assign_cache_slots()?;
    for nested in &mut bc.nested {
        // The parser produces nested `Arc<Bytecode>` with a refcount of 1, so
        // we have unique access to mutate each one in place before it ships.
        let inner =
            Arc::get_mut(nested).expect("nested Bytecode should be uniquely owned during finalize");
        finalize(inner)?;
    }
    Ok(())
}

#[cfg(test)]
mod runtime_cache_tests {
    use super::*;

    #[test]
    fn global_lookup_cache_tracks_distinct_get_global_names_only() {
        let bc = parse_str(
            r#"
            local literal = "not a global"
            local t = { field = literal }
            foo = foo + foo
            bar = bar
            "#,
        )
        .unwrap();

        let get_globals: Vec<_> = bc
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_GET_GLOBAL)
            .collect();

        assert_eq!(get_globals.len(), 3);
        assert_eq!(bc.global_cache_slots, 2);
        assert!(bc.string_literals.len() > bc.global_cache_slots as usize);
        assert_eq!(get_globals[0].bx(), get_globals[1].bx());
        assert_ne!(get_globals[0].bx(), get_globals[2].bx());
    }

    #[test]
    fn field_lookup_cache_tracks_get_field_call_sites() {
        let bc = parse_str(
            r#"
            local t = { x = 1, y = 2 }
            return t.x + t.x + t.y
            "#,
        )
        .unwrap();

        let get_fields: Vec<_> = bc
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_GET_FIELD)
            .collect();

        assert_eq!(get_fields.len(), 3);
        assert_eq!(bc.field_cache_slots, 3);
        assert_eq!(get_fields[0].bx(), 0);
        assert_eq!(get_fields[1].bx(), 1);
        assert_eq!(get_fields[2].bx(), 2);
    }

    #[test]
    fn set_field_lookup_cache_tracks_set_field_call_sites() {
        let bc = parse_str(
            r#"
            local t = { x = 0, y = 0 }
            t.x = 1
            t.x = 2
            t.y = 3
            "#,
        )
        .unwrap();

        let set_fields: Vec<_> = bc
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_SET_FIELD)
            .collect();

        assert_eq!(set_fields.len(), 3);
        assert_eq!(bc.set_field_cache_slots, 3);
        assert_eq!(set_fields[0].c(), 0);
        assert_eq!(set_fields[1].c(), 1);
        assert_eq!(set_fields[2].c(), 2);
    }
}
