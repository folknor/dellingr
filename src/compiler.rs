//! Functions and types associated with converting source code into bytecode.

mod exp_desc;
mod lexer;
mod parser;
mod token;
pub(crate) mod verify;

use std::cell::Cell;
use std::sync::Arc;

use super::Instr;
use super::Result;
use super::error;
use super::vm::{ObjectPtr, Val};

pub(crate) use parser::MAX_SYNTAX_DEPTH;

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
/// per-`State` lookup caches live in the State's per-Bytecode runtime bundle.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Bytecode {
    pub(crate) code: Vec<Instr>,
    pub(crate) number_literals: Vec<f64>,
    pub(crate) string_literals: Vec<Vec<u8>>,
    /// Table constructor templates. Each entry stores string-literal indices
    /// for a pure named-field constructor's keys, in insertion order.
    pub(crate) table_templates: Vec<Vec<u16>>,
    /// Number of slots in this function's global lookup cache.
    /// Cache slot indices are baked into `OP_GET_GLOBAL` and `OP_SET_GLOBAL`
    /// instructions.
    pub(crate) global_cache_slots: u8,
    /// Number of slots in this function's field lookup cache.
    pub(crate) field_cache_slots: u8,
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
    /// `Bytecode`. The State allocates one runtime cache bundle per Bytecode
    /// identity, sized from these counts.
    fn assign_cache_slots(&mut self) -> Result<()> {
        let mut global_cache_indices = vec![None; self.string_literals.len()];
        let mut global_cache_len = 0usize;
        let mut field_cache_len = 0usize;
        let mut set_field_cache_len = 0usize;
        let mut tfor_cursor_len = 0usize;

        for inst in &mut self.code {
            match inst.opcode() {
                Instr::OP_GET_GLOBAL => {
                    let string_idx = inst.bx() as usize;
                    let Some(cache_idx) = global_cache_indices.get_mut(string_idx) else {
                        continue;
                    };
                    let cache_idx = match *cache_idx {
                        Some(cache_idx) => cache_idx,
                        None => {
                            let next_idx = if global_cache_len < u8::MAX as usize {
                                global_cache_len as u8
                            } else {
                                u8::MAX
                            };
                            *cache_idx = Some(next_idx);
                            if next_idx != u8::MAX {
                                global_cache_len += 1;
                            }
                            next_idx
                        }
                    };
                    *inst = Instr::get_global_cached(inst.bx(), cache_idx);
                }
                Instr::OP_SET_GLOBAL => {
                    if global_cache_len < u8::MAX as usize {
                        let cache_idx = global_cache_len as u8;
                        global_cache_len += 1;
                        *inst = Instr::set_global_cached(inst.bx(), cache_idx);
                    }
                }
                Instr::OP_GET_FIELD => {
                    let cache_idx = if field_cache_len < u8::MAX as usize {
                        let cache_idx = field_cache_len as u8;
                        field_cache_len += 1;
                        cache_idx
                    } else {
                        u8::MAX
                    };
                    *inst = Instr::get_field_cached(inst.bx(), cache_idx);
                }
                Instr::OP_SET_FIELD_AT if inst.a() == 0 => {
                    let cache_idx = if set_field_cache_len < u8::MAX as usize {
                        let cache_idx = set_field_cache_len as u8;
                        set_field_cache_len += 1;
                        cache_idx
                    } else {
                        u8::MAX
                    };
                    *inst = Instr::set_field_cached(inst.bx(), cache_idx);
                }
                Instr::OP_TFOR_CALL if tfor_cursor_len < u8::MAX as usize => {
                    *inst = Instr::tfor_call_cached(inst.a(), inst.b(), tfor_cursor_len as u8);
                    tfor_cursor_len += 1;
                }
                _ => {}
            }
        }

        self.global_cache_slots = global_cache_len as u8;
        self.field_cache_slots = field_cache_len as u8;
        self.set_field_cache_slots = set_field_cache_len as u8;
        Ok(())
    }
}

fn internal_error(message: impl Into<String>) -> error::Error {
    error::Error::without_location(error::ErrorKind::InternalError(message.into()))
}

fn transfer_with_offset(inst: Instr, offset: i16) -> Option<Instr> {
    Some(match inst.opcode() {
        Instr::OP_JUMP => Instr::jump(offset),
        Instr::OP_BRANCH_FALSE => Instr::branch_false(offset),
        Instr::OP_BRANCH_TRUE_KEEP => Instr::branch_true_keep(offset),
        Instr::OP_BRANCH_FALSE_KEEP => Instr::branch_false_keep(offset),
        Instr::OP_BRANCH_FALSE_LESS => Instr::branch_false_less(offset),
        Instr::OP_BRANCH_FALSE_LESS_EQUAL => Instr::branch_false_less_equal(offset),
        Instr::OP_BRANCH_FALSE_GREATER => Instr::branch_false_greater(offset),
        Instr::OP_BRANCH_FALSE_GREATER_EQUAL => Instr::branch_false_greater_equal(offset),
        Instr::OP_BRANCH_FALSE_EQUAL => Instr::branch_false_equal(offset),
        Instr::OP_BRANCH_FALSE_NOT_EQUAL => Instr::branch_false_not_equal(offset),
        Instr::OP_FOR_PREP => Instr::for_prep(inst.a(), offset),
        Instr::OP_FOR_LOOP => Instr::for_loop(inst.a(), offset),
        Instr::OP_TFOR_LOOP => Instr::tfor_loop(inst.a(), offset),
        _ => return None,
    })
}

fn checked_remapped_offset(offset: i64) -> Result<i16> {
    i16::try_from(offset)
        .map_err(|_| internal_error("instruction stripping produced an out-of-range jump"))
}

/// Remove compiler-only no-ops and close-upvalue instructions that cannot
/// observe any open upvalues, then remap every control-transfer offset.
fn strip_dead_instructions(bc: &mut Bytecode) -> Result<()> {
    if bc.code.len() != bc.line_info.len() {
        return Err(internal_error(
            "line_info desynced from code before instruction stripping",
        ));
    }

    let has_closure = bc
        .code
        .iter()
        .any(|inst| inst.opcode() == Instr::OP_CLOSURE);
    let remove: Vec<bool> = bc
        .code
        .iter()
        .map(|inst| {
            inst.opcode() == Instr::OP_NOP
                || (!has_closure && inst.opcode() == Instr::OP_CLOSE_UPVALUES)
        })
        .collect();
    if !remove.iter().any(|removed| *removed) {
        return Ok(());
    }

    let old_len = bc.code.len();
    let mut map = Vec::with_capacity(old_len + 1);
    map.push(0usize);
    for removed in &remove {
        let retained =
            map.last().copied().expect("boundary map starts non-empty") + usize::from(!*removed);
        map.push(retained);
    }
    let retained_len = *map.last().expect("boundary map starts non-empty");

    for (source, inst) in bc.code.iter_mut().enumerate() {
        let Some(_) = transfer_with_offset(*inst, 0) else {
            continue;
        };
        let old_next = source + 1;
        let old_target = if inst.sbx() >= 0 {
            old_next.checked_add(inst.sbx() as usize)
        } else {
            old_next.checked_sub(inst.sbx().unsigned_abs() as usize)
        }
        .ok_or_else(|| internal_error("instruction stripping found an out-of-range jump"))?;
        if old_target >= old_len {
            return Err(internal_error(
                "instruction stripping found an out-of-range jump",
            ));
        }
        let new_target = map[old_target];
        if new_target >= retained_len {
            return Err(internal_error(
                "instruction stripping mapped a jump beyond retained code",
            ));
        }
        let new_offset = checked_remapped_offset(new_target as i64 - map[old_next] as i64)?;
        *inst = transfer_with_offset(*inst, new_offset)
            .expect("transfer opcode was recognized before remapping");
    }

    let old_code = std::mem::take(&mut bc.code);
    let old_line_info = std::mem::take(&mut bc.line_info);
    for ((inst, line), removed) in old_code.into_iter().zip(old_line_info).zip(remove) {
        if !removed {
            bc.code.push(inst);
            bc.line_info.push(line);
        }
    }
    Ok(())
}

/// Per-(State, Bytecode) lookup caches owned by the State runtime bundle.
///
/// These are never shared across `State`s: cached `ObjectPtr` keys are only
/// valid inside the heap of the State that wrote them, and version cells are
/// keyed to that State's `globals_version` / table versions. They are never
/// serialized and are rebuilt cold when a snapshot is loaded.
///
/// The interior `Cell`s give cache writes a `&self`-only borrow shape, which
/// matches the dispatch loop's invariants: simultaneous frames executing the
/// same State-local runtime bundle all see each other's writes.
#[derive(Debug, Default)]
pub(crate) struct RuntimeCaches {
    pub(super) global_lookup: Vec<GlobalLookupCacheSlot>,
    pub(super) field_lookup: Vec<FieldLookupCacheSlot>,
    pub(super) set_field_lookup: Vec<SetFieldLookupCacheSlot>,
    pub(super) tfor_cursor: Vec<TforCursorSlot>,
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
            // Cursor slots deliberately have no Bytecode count: the three
            // existing cache counts are snapshot bytes, and a fourth would
            // change format 6. Verified C operands make this high-water mark
            // the exact allocation count.
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

/// Finalize a freshly-parsed `Bytecode` tree before it ships to the runtime.
fn finalize(bc: &mut Bytecode) -> Result<()> {
    debug_assert_eq!(
        bc.code.len(),
        bc.line_info.len(),
        "line_info desynced from code"
    );
    strip_dead_instructions(bc)?;
    debug_assert_eq!(
        bc.code.len(),
        bc.line_info.len(),
        "line_info desynced from code after instruction stripping"
    );
    debug_assert!(
        bc.code.iter().all(|inst| inst.opcode() != Instr::OP_NOP),
        "finalized compiler output must not contain OP_NOP"
    );
    bc.assign_cache_slots()?;
    // Runs in release too, and the result is propagated rather than dropped.
    // Verifying our own fresh output guards against a compiler bug, and the
    // only reason to pay for it is to act on it: a debug-only assertion would
    // let a release build emit stack-imbalanced bytecode and discover it later
    // as a VM panic, which is precisely the failure mode this crate refuses.
    // Reporting it as a compile error keeps a compiler bug catchable instead of
    // fatal. The debug assertion stays on top so CI fails loudly at the source
    // rather than surfacing it as a user-facing error.
    if let Err(error) = verify::validate_bytecode(bc) {
        debug_assert!(
            false,
            "compiler emitted bytecode rejected by the snapshot verifier: {error:?}"
        );
        return Err(error::Error::without_location(
            error::ErrorKind::InternalError(format!(
                "compiler emitted invalid bytecode: {}",
                error.reason
            )),
        ));
    }
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

    /// The dynamic shapes the verifier's `Known`/`Dyn` transitions and marker
    /// stacks exist for. If the corpus stops containing one of these, the
    /// acceptance proof silently stops covering the interesting half of the
    /// analysis, so the corpus test pins that each was actually seen.
    #[derive(Default)]
    struct DynamicShapes {
        mark_call_base: bool,
        dynamic_call: bool,
        new_table_tracked: bool,
        dynamic_set_list: bool,
        return_all: bool,
        vararg_all: bool,
    }

    fn assert_verified_tree(bytecode: &Bytecode, seen: &mut DynamicShapes) {
        verify::validate_bytecode(bytecode)
            .expect("compiler output must pass bytecode verification");
        for instruction in &bytecode.code {
            match instruction.opcode() {
                Instr::OP_MARK_CALL_BASE => seen.mark_call_base = true,
                Instr::OP_NEW_TABLE_TRACKED => seen.new_table_tracked = true,
                Instr::OP_CALL if instruction.a() == u8::MAX => seen.dynamic_call = true,
                Instr::OP_SET_LIST if instruction.a() == 0 => seen.dynamic_set_list = true,
                Instr::OP_RETURN if instruction.a() == u8::MAX => seen.return_all = true,
                Instr::OP_VARARG if instruction.a() == u8::MAX => seen.vararg_all = true,
                _ => {}
            }
        }
        for nested in &bytecode.nested {
            assert_verified_tree(nested, seen);
        }
    }

    fn collect_lua_sources(directory: &std::path::Path, paths: &mut Vec<std::path::PathBuf>) {
        let entries = std::fs::read_dir(directory).expect("examples directory must be readable");
        for entry in entries {
            let path = entry
                .expect("example directory entry must be readable")
                .path();
            if path.is_dir() {
                collect_lua_sources(&path, paths);
            } else if path.extension().is_some_and(|extension| extension == "lua") {
                paths.push(path);
            }
        }
    }

    #[test]
    fn compiler_examples_pass_stack_discipline_verification() {
        let mut paths = Vec::new();
        collect_lua_sources(std::path::Path::new("examples"), &mut paths);
        paths.sort();
        // Without this the corpus proof passes vacuously if the walk finds
        // nothing, which is exactly the failure mode that would let a verifier
        // that rejects real compiler output reach a release.
        assert!(
            paths.len() > 20,
            "the example corpus is the proof that the verifier accepts real \
             compiler output; found only {} sources",
            paths.len()
        );
        let mut seen = DynamicShapes::default();
        for path in paths {
            let source = std::fs::read_to_string(&path).expect("example source must be readable");
            let bytecode = parse_str(&source).expect("example source must compile");
            assert_verified_tree(&bytecode, &mut seen);
        }
        assert!(seen.mark_call_base, "corpus lost OP_MARK_CALL_BASE");
        assert!(seen.dynamic_call, "corpus lost dynamic OP_CALL");
        assert!(seen.new_table_tracked, "corpus lost OP_NEW_TABLE_TRACKED");
        assert!(seen.dynamic_set_list, "corpus lost dynamic OP_SET_LIST");
        assert!(seen.return_all, "corpus lost OP_RETURN with RetCount::All");
        assert!(seen.vararg_all, "corpus lost OP_VARARG with all varargs");
    }

    #[test]
    fn global_lookup_cache_shares_gets_and_assigns_fresh_set_slots() {
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
        let set_globals: Vec<_> = bc
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_SET_GLOBAL)
            .collect();

        assert_eq!(set_globals.len(), 2);
        // 2 distinct read names (shared by literal) + 2 fresh SET sites; the
        // two non-global literals ("not a global", "field") hold no slot -
        // they would push this to 6 if literal count drove allocation.
        assert_eq!(bc.global_cache_slots, 4);
        assert_eq!(get_globals[0].a(), get_globals[1].a());
        assert_ne!(get_globals[0].a(), get_globals[2].a());
        assert_eq!(set_globals[0].a(), 2);
        assert_eq!(set_globals[1].a(), 4);
    }

    #[test]
    fn global_set_cache_uses_biased_fresh_slots_and_excludes_builtins() {
        let bc = parse_str("foo = 1; foo = 2; table = {}").expect("source compiles");
        let set_globals: Vec<_> = bc
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_SET_GLOBAL)
            .collect();

        assert_eq!(bc.global_cache_slots, 2);
        assert_eq!(set_globals.len(), 2);
        assert_eq!(set_globals[0].a(), 1);
        assert_eq!(set_globals[1].a(), 2);
        assert!(
            bc.code
                .iter()
                .any(|inst| inst.opcode() == Instr::OP_SET_BUILTIN)
        );
    }

    #[test]
    fn global_set_cache_leaves_the_256th_site_uncached() {
        let source = (0..256)
            .map(|idx| format!("set_cache_{idx} = {idx}"))
            .collect::<Vec<_>>()
            .join("; ");
        let bc = parse_str(source).expect("source compiles");
        let set_globals: Vec<_> = bc
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_SET_GLOBAL)
            .collect();

        assert_eq!(bc.global_cache_slots, u8::MAX);
        assert_eq!(set_globals.len(), 256);
        for (idx, inst) in set_globals.iter().take(255).enumerate() {
            assert_eq!(inst.a(), idx as u8 + 1);
        }
        assert_eq!(set_globals[255].a(), 0);
    }

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
        assert_eq!(get_fields[0].a(), 0);
        assert_eq!(get_fields[1].a(), 1);
        assert_eq!(get_fields[2].a(), 2);
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
        assert_eq!(set_fields[0].a(), 0);
        assert_eq!(set_fields[1].a(), 1);
        assert_eq!(set_fields[2].a(), 2);
    }

    fn transfer(opcode: u8, offset: i16) -> Instr {
        match opcode {
            Instr::OP_JUMP => Instr::jump(offset),
            Instr::OP_BRANCH_FALSE => Instr::branch_false(offset),
            Instr::OP_BRANCH_TRUE_KEEP => Instr::branch_true_keep(offset),
            Instr::OP_BRANCH_FALSE_KEEP => Instr::branch_false_keep(offset),
            Instr::OP_FOR_PREP => Instr::for_prep(7, offset),
            Instr::OP_FOR_LOOP => Instr::for_loop(7, offset),
            Instr::OP_TFOR_LOOP => Instr::tfor_loop(7, offset),
            _ => panic!("test fixture requires a transfer opcode"),
        }
    }

    #[test]
    fn strip_remaps_every_transfer_forward_and_backward() {
        let transfer_opcodes = [
            Instr::OP_JUMP,
            Instr::OP_BRANCH_FALSE,
            Instr::OP_BRANCH_TRUE_KEEP,
            Instr::OP_BRANCH_FALSE_KEEP,
            Instr::OP_FOR_PREP,
            Instr::OP_FOR_LOOP,
            Instr::OP_TFOR_LOOP,
        ];
        for opcode in transfer_opcodes {
            let mut forward = Bytecode {
                code: vec![
                    transfer(opcode, 2),
                    Instr::nop(),
                    Instr::push_nil(),
                    Instr::ret(crate::RetCount::Fixed(0)),
                ],
                line_info: vec![10, 11, 12, 13],
                ..Bytecode::default()
            };
            strip_dead_instructions(&mut forward).expect("forward transfer strips");
            assert_eq!(forward.code[0].sbx(), 1, "opcode {opcode}");
            assert_eq!(
                forward.code[0].a(),
                if opcode >= Instr::OP_FOR_PREP { 7 } else { 0 }
            );
            assert_eq!(forward.line_info, vec![10, 12, 13]);

            let mut backward = Bytecode {
                code: vec![
                    Instr::push_nil(),
                    Instr::nop(),
                    Instr::push_nil(),
                    transfer(opcode, -4),
                    Instr::ret(crate::RetCount::Fixed(0)),
                ],
                line_info: vec![20, 21, 22, 23, 24],
                ..Bytecode::default()
            };
            strip_dead_instructions(&mut backward).expect("backward transfer strips");
            assert_eq!(backward.code[2].sbx(), -3, "opcode {opcode}");
            assert_eq!(
                backward.code[2].a(),
                if opcode >= Instr::OP_FOR_PREP { 7 } else { 0 }
            );
            assert_eq!(backward.line_info, vec![20, 22, 23, 24]);
        }
    }

    #[test]
    fn strip_handles_removed_targets_consecutive_regions_and_close_upvalues() {
        let mut bytecode = Bytecode {
            code: vec![
                Instr::jump(2),
                Instr::nop(),
                Instr::close_upvalues(0),
                Instr::push_nil(),
                Instr::ret(crate::RetCount::Fixed(0)),
            ],
            line_info: vec![1, 2, 3, 4, 5],
            ..Bytecode::default()
        };
        strip_dead_instructions(&mut bytecode).expect("mixed dead region strips");
        assert_eq!(
            bytecode.code,
            vec![
                Instr::jump(0),
                Instr::push_nil(),
                Instr::ret(crate::RetCount::Fixed(0))
            ]
        );
        assert_eq!(bytecode.line_info, vec![1, 4, 5]);

        let mut removed_target = Bytecode {
            code: vec![
                Instr::jump(1),
                Instr::push_nil(),
                Instr::nop(),
                Instr::push_nil(),
                Instr::ret(crate::RetCount::Fixed(0)),
            ],
            line_info: vec![6, 7, 8, 9, 10],
            ..Bytecode::default()
        };
        strip_dead_instructions(&mut removed_target).expect("removed target remaps");
        assert_eq!(removed_target.code[0], Instr::jump(1));
        assert_eq!(removed_target.line_info, vec![6, 7, 9, 10]);

        let mut removal_before_endpoints = Bytecode {
            code: vec![
                Instr::nop(),
                Instr::push_nil(),
                Instr::jump(1),
                Instr::push_nil(),
                Instr::ret(crate::RetCount::Fixed(0)),
            ],
            line_info: vec![11, 12, 13, 14, 15],
            ..Bytecode::default()
        };
        strip_dead_instructions(&mut removal_before_endpoints)
            .expect("removal before endpoints remaps");
        assert_eq!(removal_before_endpoints.code[1], Instr::jump(1));
        assert_eq!(removal_before_endpoints.line_info, vec![12, 13, 14, 15]);

        let mut containing_closure = Bytecode {
            code: vec![
                Instr::close_upvalues(0),
                Instr::nop(),
                Instr::closure(0),
                Instr::ret(crate::RetCount::Fixed(0)),
            ],
            line_info: vec![1, 2, 3, 4],
            ..Bytecode::default()
        };
        strip_dead_instructions(&mut containing_closure).expect("nop strips around closure");
        assert_eq!(containing_closure.code[0], Instr::close_upvalues(0));
        assert_eq!(containing_closure.line_info, vec![1, 3, 4]);
    }

    #[test]
    fn strip_rejects_bad_targets_and_unrepresentable_offsets() {
        let mut bad_target = Bytecode {
            code: vec![
                Instr::jump(2),
                Instr::nop(),
                Instr::ret(crate::RetCount::Fixed(0)),
            ],
            line_info: vec![1, 2, 3],
            ..Bytecode::default()
        };
        assert!(strip_dead_instructions(&mut bad_target).is_err());
        assert!(checked_remapped_offset(i64::from(i16::MAX) + 1).is_err());
    }

    fn assert_no_opcode_in_tree(bytecode: &Bytecode, opcode: u8) {
        assert!(bytecode.code.iter().all(|inst| inst.opcode() != opcode));
        for nested in &bytecode.nested {
            assert_no_opcode_in_tree(nested, opcode);
        }
    }

    #[test]
    fn fixed_calls_become_raw_nops_then_finalize_strips_them() {
        let raw = parser::parse_str("f(1); obj:m(2)").expect("raw parser output compiles");
        assert_eq!(
            raw.code
                .iter()
                .filter(|inst| inst.opcode() == Instr::OP_NOP)
                .count(),
            2
        );
        let finalized = parse_str("f(1); obj:m(2)").expect("finalized output compiles");
        assert_no_opcode_in_tree(&finalized, Instr::OP_NOP);

        for source in ["local function f(...) end; f(...)", "f(g())"] {
            let dynamic = parser::parse_str(source).expect("raw dynamic call compiles");
            assert!(
                dynamic
                    .code
                    .iter()
                    .any(|inst| inst.opcode() == Instr::OP_MARK_CALL_BASE),
                "{source} must retain the dynamic call marker"
            );
        }
    }

    #[test]
    fn finalize_strips_close_upvalues_per_chunk_independently() {
        let raw = parser::parse_str(
            "local function plain() do local x = 1 end end; local function captures() do local x = 1; f = function() return x end end end",
        )
        .expect("raw nested functions compile");
        assert!(
            raw.nested[0]
                .code
                .iter()
                .any(|inst| inst.opcode() == Instr::OP_CLOSE_UPVALUES)
        );

        let finalized = parse_str(
            "local function plain() do local x = 1 end end; local function captures() do local x = 1; f = function() return x end end end",
        )
        .expect("finalized nested functions compile");
        assert_no_opcode_in_tree(&finalized.nested[0], Instr::OP_CLOSE_UPVALUES);
        assert!(
            finalized.nested[1]
                .code
                .iter()
                .any(|inst| inst.opcode() == Instr::OP_CLOSE_UPVALUES)
        );
    }

    #[test]
    fn stripping_preserves_costs_and_decreases_only_instruction_counts() {
        let mut raw =
            parser::parse_str("local x = 0; for i = 1, 3 do do local y = i; x = x + y end end")
                .expect("raw cost fixture compiles");
        let before = crate::ScopeCost::analyze_chunk(&raw, "main".to_string());
        finalize(&mut raw).expect("cost fixture finalizes");
        let after = crate::ScopeCost::analyze_chunk(&raw, "main".to_string());
        assert_eq!(after.own_cost, before.own_cost);
        assert_eq!(after.total_cost, before.total_cost);
        assert_eq!(after.arithmetic_ops, before.arithmetic_ops);
        assert_eq!(after.table_creations, before.table_creations);
        assert_eq!(after.table_writes, before.table_writes);
        assert!(after.instructions < before.instructions);
    }
}
