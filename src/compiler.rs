//! Functions and types associated with converting source code into bytecode.

#![cfg_attr(
    not(test),
    deny(clippy::panic, clippy::unreachable, clippy::expect_used)
)]

mod exp_desc;
mod lexer;
mod parser;
mod token;
pub(crate) mod verify;

use std::sync::Arc;

use super::Instr;
use super::Result;
use super::error;
use crate::instr::MAX_CACHE_SLOTS;

/// Describes where an upvalue comes from when creating a closure.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum UpvalueDesc {
    /// Capture a local variable from the immediately enclosing function.
    Local(u8),
    /// Capture an upvalue from the immediately enclosing function.
    Upvalue(u8),
}

/// Compiled, immutable bytecode for a single Lua function (chunk).
///
/// Execution trusts the static contracts checked by finalization and snapshot
/// admission. This raw representation is also needed by the parser, verifier
/// fixtures and snapshot reconstruction. Wrapping only Frame's Arc would not
/// prove those producers verified a chunk; such a capability must cover all
/// producers and the nested graph, rather than revalidate on recursive calls.
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
    /// Number of slots in this function's field lookup cache (`OP_GET_FIELD`).
    pub(crate) field_cache_slots: u8,
    /// Number of slots in this function's set-field lookup cache
    /// (`OP_SET_FIELD`).
    pub(crate) set_field_cache_slots: u8,
    /// Number of `pairs` cursor slots (`OP_TFOR_CALL`).
    pub(crate) tfor_cursor_slots: u8,
    pub(crate) num_params: u8,
    pub(crate) num_locals: u8,
    pub(crate) nested: Vec<Arc<Bytecode>>,
    /// Describes the upvalues this function captures.
    pub(crate) upvalues: Vec<UpvalueDesc>,
    /// Whether this function accepts varargs (...).
    pub(crate) is_vararg: bool,
    /// Optional function name (for debugging/analysis), shared with the
    /// trace frames and VM faults that report it.
    pub(crate) name: Option<Arc<str>>,
    /// Shared chunk name (file path or other identifier), byte-exact as the
    /// host gave it to `Program::compile`.
    pub(crate) source: Option<Arc<[u8]>>,
    /// Maps instruction index to source line number.
    /// line_info[i] is the line number for code[i].
    pub(crate) line_info: Vec<u32>,
}

impl Bytecode {
    /// Walk the instruction stream, rewrite cache-able opcodes with their
    /// allocated slot index, and record per-cache slot counts on the
    /// `Bytecode`. The State allocates one runtime cache bundle per Bytecode
    /// identity, sized from these counts. Cache operands are persisted as raw
    /// instruction words, so changing their encoding (the `Instr` cache-slot
    /// codec) requires a snapshot format version bump. The verifier restates
    /// this allocation policy on purpose; see `validate_bytecode`.
    ///
    /// Every cache-bearing operand is rewritten, whatever it held before, so
    /// the pass is idempotent: an already-finalized `SET_FIELD` is allocated
    /// again exactly like the offset-zero `SET_FIELD_AT` it came from.
    fn assign_cache_slots(&mut self) {
        /// Allocate the next slot of one cache family, or `None` once the
        /// family holds `MAX_CACHE_SLOTS` slots.
        fn allocate(len: &mut usize) -> Option<u8> {
            if *len >= MAX_CACHE_SLOTS {
                return None;
            }
            // Below MAX_CACHE_SLOTS, so it fits a u8.
            let slot = *len as u8;
            *len += 1;
            Some(slot)
        }

        // Per string literal: `None` until the name's first GET_GLOBAL, then
        // that reference's slot, `Some(None)` when the cache was exhausted
        // there (every later read of the name stays uncached as well).
        let mut global_slots: Vec<Option<Option<u8>>> = vec![None; self.string_literals.len()];
        let mut global_len = 0usize;
        let mut field_len = 0usize;
        let mut set_field_len = 0usize;
        let mut tfor_cursor_len = 0usize;

        for inst in &mut self.code {
            match inst.opcode() {
                Instr::OP_GET_GLOBAL => {
                    // An out-of-range literal id has no slot entry; it stays
                    // uncached and the verifier rejects it right after.
                    let slot = global_slots
                        .get_mut(usize::from(inst.bx()))
                        .and_then(|entry| *entry.get_or_insert_with(|| allocate(&mut global_len)));
                    *inst = Instr::get_global(inst.bx(), slot);
                }
                // Each SET_GLOBAL site takes a fresh slot of the shared global
                // cache, never one a GET_GLOBAL name owns.
                Instr::OP_SET_GLOBAL => {
                    *inst = Instr::set_global(inst.bx(), allocate(&mut global_len));
                }
                Instr::OP_GET_FIELD => {
                    *inst = Instr::get_field(inst.bx(), allocate(&mut field_len));
                }
                Instr::OP_SET_FIELD => {
                    *inst = Instr::set_field(inst.bx(), allocate(&mut set_field_len));
                }
                // A nonzero SET_FIELD_AT offset names a deeper receiver and is
                // never cached.
                Instr::OP_SET_FIELD_AT if inst.a() == 0 => {
                    *inst = Instr::set_field(inst.bx(), allocate(&mut set_field_len));
                }
                Instr::OP_TFOR_CALL => {
                    *inst = Instr::tfor_call(inst.a(), inst.b(), allocate(&mut tfor_cursor_len));
                }
                _ => {}
            }
        }

        // `allocate` caps every family at MAX_CACHE_SLOTS, which fits a u8.
        self.global_cache_slots = global_len as u8;
        self.field_cache_slots = field_len as u8;
        self.set_field_cache_slots = set_field_len as u8;
        self.tfor_cursor_slots = tfor_cursor_len as u8;
    }
}

fn internal_error(fault: error::CompilerFault) -> error::Error {
    error::Error::internal_fault(error::InternalFault::Compiler(fault))
}

fn transfer_with_offset(inst: Instr, offset: i16) -> Option<Instr> {
    Instr::is_transfer(inst.opcode()).then(|| Instr::op_a_sbx(inst.opcode(), inst.a(), offset))
}

fn checked_remapped_offset(offset: i64) -> Result<i16> {
    i16::try_from(offset)
        .map_err(|_| internal_error(error::CompilerFault::RemappedJumpOffsetOutOfRange))
}

/// Remove compiler-only no-ops, then remap every control-transfer offset.
/// Close-upvalue instructions that cannot observe an open upvalue are never
/// emitted (or are NOP'd at loop exit) by the parser's per-scope capture log.
fn strip_dead_instructions(bc: &mut Bytecode) -> Result<()> {
    let remove: Vec<bool> = bc
        .code
        .iter()
        .map(|inst| inst.opcode() == Instr::OP_NOP)
        .collect();
    if !remove.iter().any(|removed| *removed) {
        return Ok(());
    }

    let old_len = bc.code.len();
    if bc.line_info.len() != old_len {
        return Err(internal_error(
            error::CompilerFault::MissingLineForInstruction,
        ));
    }
    let mut map = Vec::with_capacity(old_len + 1);
    let mut retained_len = 0usize;
    map.push(retained_len);
    for removed in &remove {
        retained_len += usize::from(!*removed);
        map.push(retained_len);
    }

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
        .ok_or_else(|| internal_error(error::CompilerFault::JumpTargetOutOfRange))?;
        if old_target >= old_len {
            return Err(internal_error(error::CompilerFault::JumpTargetOutOfRange));
        }
        let new_target = map[old_target];
        if new_target >= retained_len {
            return Err(internal_error(error::CompilerFault::RemappedJumpBeyondCode));
        }
        let new_offset = checked_remapped_offset(new_target as i64 - map[old_next] as i64)?;
        *inst = transfer_with_offset(*inst, new_offset)
            .ok_or_else(|| internal_error(error::CompilerFault::TransferOpcodeExpected))?;
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

/// The one compile path: Lua source bytes plus an optional, byte-exact chunk
/// name, to finalized bytecode. The name is bounded before anything else is
/// examined, so an oversized name wins over a malformed or oversized source.
#[hotpath::measure]
pub(crate) fn compile(source: impl AsRef<[u8]>, name: Option<&[u8]>) -> Result<Bytecode> {
    if let Some(name) = name
        && name.len() > crate::limits::MAX_SOURCE_NAME_BYTES
    {
        return Err(error::Error::without_location(
            error::ErrorKind::SourceNameSizeExceeded {
                size: name.len(),
                limit: crate::limits::MAX_SOURCE_NAME_BYTES,
            },
        ));
    }
    let mut bc = parser::parse(source.as_ref(), name.map(Arc::<[u8]>::from))?;
    finalize(&mut bc)?;
    Ok(bc)
}

/// Finalize a freshly-parsed `Bytecode` tree before it ships to the runtime.
fn finalize(bc: &mut Bytecode) -> Result<()> {
    finalize_at_depth(bc, 1)
}

fn validation_error(bc: &Bytecode, validation: verify::BytecodeValidationError) -> error::Error {
    // Internal errors start without a location. The error representation uses
    // 0:0 for that state; attach a line only when the verifier identifies one.
    let line = validation
        .instruction
        .and_then(|instruction| bc.line_info.get(instruction as usize))
        .copied();
    let mut error = error::Error::internal_fault(error::InternalFault::CompilerValidation {
        function: bc.name.clone(),
        error: validation,
    })
    .with_source_name(bc.source.clone());
    debug_assert!(false, "{error}");
    if let Some(line) = line {
        error = error.with_location(line, 0);
    }
    error
}

fn finalize_at_depth(bc: &mut Bytecode, depth: usize) -> Result<()> {
    verify::validate_depth(depth).map_err(|error| validation_error(bc, error))?;
    // Diagnostic names are not identifiers used by execution. Bound them at
    // compilation so every emitted chunk fits the unchanged snapshot field.
    if let Some(name) = &bc.name
        && name.len() > crate::limits::MAX_CHUNK_NAME_BYTES
    {
        let mut end = crate::limits::MAX_CHUNK_NAME_BYTES;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        bc.name = Some(Arc::from(&name[..end]));
    }
    if bc.code.len() != bc.line_info.len() {
        return Err(internal_error(error::CompilerFault::LineInfoDesynced));
    }
    strip_dead_instructions(bc).map_err(|error| error.with_source_name(bc.source.clone()))?;
    debug_assert_eq!(
        bc.code.len(),
        bc.line_info.len(),
        "line_info desynced from code after instruction stripping"
    );
    debug_assert!(
        bc.code.iter().all(|inst| inst.opcode() != Instr::OP_NOP),
        "finalized compiler output must not contain OP_NOP"
    );
    bc.assign_cache_slots();
    // Runs in release too, and the result is propagated rather than dropped.
    // Verifying our own fresh output guards against a compiler bug, and the
    // only reason to pay for it is to act on it: a debug-only assertion would
    // let a release build emit stack-imbalanced bytecode and discover it later
    // as a VM panic, which is precisely the failure mode this crate refuses.
    // Reporting it as a compile error keeps a compiler bug catchable instead of
    // fatal. Line metadata is checked before stripping so a mismatch cannot
    // silently truncate the instruction stream while the vectors are zipped.
    verify::validate_bytecode(bc).map_err(|error| validation_error(bc, error))?;
    for child in &bc.nested {
        verify::validate_child(bc, child.as_ref()).map_err(|error| validation_error(bc, error))?;
    }
    for nested in &mut bc.nested {
        // The parser produces nested `Arc<Bytecode>` with a refcount of 1, so
        // we have unique access to mutate each one in place before it ships.
        let inner = Arc::get_mut(nested)
            .ok_or_else(|| internal_error(error::CompilerFault::NestedBytecodeNotUniquelyOwned))?;
        finalize_at_depth(inner, depth + 1)?;
    }
    Ok(())
}

#[cfg(test)]
mod runtime_cache_tests {
    use super::*;
    use crate::error::{ErrorKind, SyntaxError};
    use crate::limits::{MAX_SYNTAX_DEPTH, MAX_VERIFIER_MARKER_DEPTH, SYNTAX_SUFFIX_COST};

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
                Instr::OP_CALL if instruction.call_args() == crate::ArgCount::Dynamic => {
                    seen.dynamic_call = true;
                }
                Instr::OP_SET_LIST if instruction.a() == 0 => seen.dynamic_set_list = true,
                Instr::OP_RETURN if instruction.return_count() == crate::RetCount::All => {
                    seen.return_all = true;
                }
                Instr::OP_VARARG if instruction.vararg_count() == crate::RetCount::All => {
                    seen.vararg_all = true;
                }
                _ => {}
            }
        }
        for nested in &bytecode.nested {
            assert_verified_tree(nested, seen);
        }
    }

    /// The integration tests' examples-corpus walker, compiled into this unit
    /// test so both share one traversal and ordering policy.
    mod examples_corpus {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/common/examples.rs"
        ));
    }

    fn deepest_accepted_program(source_for_depth: impl Fn(usize) -> String) -> (usize, Bytecode) {
        let mut deepest = None;
        let mut first_rejected = None;
        for depth in 1..=(MAX_SYNTAX_DEPTH as usize + 1) {
            let source = source_for_depth(depth);
            match compile(&source, None) {
                Ok(bytecode) => deepest = Some((depth, bytecode)),
                Err(error) => {
                    assert!(matches!(
                        error.kind(),
                        ErrorKind::SyntaxError(SyntaxError::TooManySyntaxLevels)
                    ));
                    first_rejected = Some(depth);
                    break;
                }
            }
        }

        let (depth, bytecode) = deepest.expect("a simple nested program must compile");
        assert_eq!(first_rejected, Some(depth + 1));
        (depth, bytecode)
    }

    fn marker_peak(
        bytecode: &Bytecode,
        open_opcode: u8,
        closes_marker: impl Fn(&Instr) -> bool,
    ) -> usize {
        let mut marker_depth = 0usize;
        let mut peak_marker_depth = 0usize;
        for instruction in &bytecode.code {
            if instruction.opcode() == open_opcode {
                marker_depth += 1;
                peak_marker_depth = peak_marker_depth.max(marker_depth);
            } else if closes_marker(instruction) {
                assert!(marker_depth > 0, "closing opcodes must have a marker");
                marker_depth -= 1;
            }
        }
        assert_eq!(marker_depth, 0, "all markers must be closed");
        peak_marker_depth
    }

    #[test]
    fn deepest_nested_programs_fit_verifier_marker_limit() {
        std::thread::Builder::new()
            .stack_size(parser::DEEP_PARSE_TEST_STACK_BYTES)
            .spawn(deepest_nested_programs_body)
            .expect("spawning the marker limit test thread should succeed")
            .join()
            .expect("marker limit test thread should not panic");
    }

    fn deepest_nested_programs_body() {
        let (call_depth, call_chain) = deepest_accepted_program(|depth| {
            format!("return {}{}", "f(".repeat(depth), ")".repeat(depth))
        });
        // The root statement list charges one unit; each call level charges
        // its expression and unary units plus the suffix charge. The innermost
        // call has no arguments, so it adds no expression units of its own.
        assert_eq!(
            call_depth,
            (MAX_SYNTAX_DEPTH as usize - 1) / (2 + SYNTAX_SUFFIX_COST as usize),
            "the call chain must stop where the syntax budget says it does"
        );
        let call_marker_peak = marker_peak(&call_chain, Instr::OP_MARK_CALL_BASE, |instruction| {
            instruction.opcode() == Instr::OP_CALL
                && instruction.call_args() == crate::ArgCount::Dynamic
        });
        assert!(
            call_marker_peak <= MAX_VERIFIER_MARKER_DEPTH,
            "compiler emitted {call_marker_peak} nested call markers at call depth {call_depth}"
        );

        let (table_depth, table_chain) = deepest_accepted_program(|depth| {
            let mut expression = "{g()}".to_string();
            for _ in 1..depth {
                expression = format!("{{f({expression})}}");
            }
            format!("return {expression}")
        });
        let table_marker_peak =
            marker_peak(&table_chain, Instr::OP_NEW_TABLE_TRACKED, |instruction| {
                instruction.opcode() == Instr::OP_SET_LIST && instruction.a() == 0
            });
        assert!(
            table_marker_peak <= MAX_VERIFIER_MARKER_DEPTH,
            "compiler emitted {table_marker_peak} nested table markers at table depth {table_depth}"
        );
    }

    #[test]
    fn compiler_examples_pass_stack_discipline_verification() {
        let paths = examples_corpus::example_corpus();
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
            let bytecode = compile(&source, None).expect("example source must compile");
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
        let bc = compile(
            r#"
            local literal = "not a global"
            local t = { field = literal }
            foo = foo + foo
            bar = bar
            "#,
            None,
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
        assert_eq!(get_globals[0].global_cache_slot(), Some(0));
        assert_eq!(get_globals[1].global_cache_slot(), Some(0));
        assert_eq!(get_globals[2].global_cache_slot(), Some(2));
        assert_eq!(set_globals[0].global_cache_slot(), Some(1));
        assert_eq!(set_globals[1].global_cache_slot(), Some(3));
    }

    #[test]
    fn global_set_cache_uses_biased_fresh_slots_and_excludes_builtins() {
        let bc = compile("foo = 1; foo = 2; table = {}", None).expect("source compiles");
        let set_globals: Vec<_> = bc
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_SET_GLOBAL)
            .collect();

        assert_eq!(bc.global_cache_slots, 2);
        assert_eq!(set_globals.len(), 2);
        assert_eq!(set_globals[0].global_cache_slot(), Some(0));
        assert_eq!(set_globals[1].global_cache_slot(), Some(1));
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
        let bc = compile(source, None).expect("source compiles");
        let set_globals: Vec<_> = bc
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_SET_GLOBAL)
            .collect();

        assert_eq!(usize::from(bc.global_cache_slots), MAX_CACHE_SLOTS);
        assert_eq!(set_globals.len(), 256);
        for (idx, inst) in set_globals.iter().take(255).enumerate() {
            assert_eq!(inst.global_cache_slot(), Some(idx as u8));
        }
        assert_eq!(set_globals[255].global_cache_slot(), None);
    }

    #[test]
    fn field_lookup_cache_tracks_get_field_call_sites() {
        let bc = compile(
            r#"
            local t = { x = 1, y = 2 }
            return t.x + t.x + t.y
            "#,
            None,
        )
        .unwrap();

        let get_fields: Vec<_> = bc
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_GET_FIELD)
            .collect();

        assert_eq!(get_fields.len(), 3);
        assert_eq!(bc.field_cache_slots, 3);
        for (slot, inst) in get_fields.iter().enumerate() {
            assert_eq!(inst.field_cache_slot(), Some(slot as u8));
        }
    }

    #[test]
    fn set_field_lookup_cache_tracks_set_field_call_sites() {
        let bc = compile(
            r#"
            local t = { x = 0, y = 0 }
            t.x = 1
            t.x = 2
            t.y = 3
            "#,
            None,
        )
        .unwrap();

        let set_fields: Vec<_> = bc
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_SET_FIELD)
            .collect();

        assert_eq!(set_fields.len(), 3);
        assert_eq!(bc.set_field_cache_slots, 3);
        for (slot, inst) in set_fields.iter().enumerate() {
            assert_eq!(inst.set_field_cache_slot(), Some(slot as u8));
        }
    }

    #[test]
    fn cache_assignment_is_idempotent() {
        let source = "local t = {}; for k in pairs(t) do t.x = g.y end; h = t.x; local a, b = {}, {}; a.f, b.f = 1, 2";
        let once = compile(source, None).expect("source compiles");
        let mut twice = once.clone();
        twice.assign_cache_slots();
        assert_eq!(twice, once);
        assert!(once.set_field_cache_slots > 0 && once.tfor_cursor_slots == 1);
    }

    fn transfer(opcode: u8, offset: i16) -> Instr {
        assert!(Instr::is_transfer(opcode));
        let a = if matches!(
            opcode,
            Instr::OP_FOR_PREP | Instr::OP_FOR_LOOP | Instr::OP_TFOR_LOOP
        ) {
            7
        } else {
            0
        };
        Instr::op_a_sbx(opcode, a, offset)
    }

    #[test]
    fn strip_remaps_every_transfer_forward_and_backward() {
        for opcode in (u8::MIN..=u8::MAX).filter(|opcode| Instr::is_transfer(*opcode)) {
            let expected_a = transfer(opcode, 0).a();
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
            assert_eq!(forward.code[0].a(), expected_a);
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
            assert_eq!(backward.code[2].a(), expected_a);
            assert_eq!(backward.line_info, vec![20, 22, 23, 24]);
        }
    }

    #[test]
    fn strip_handles_removed_targets_and_consecutive_regions() {
        let mut bytecode = Bytecode {
            code: vec![
                Instr::jump(2),
                Instr::nop(),
                Instr::nop(),
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

        let mut close_kept = Bytecode {
            code: vec![
                Instr::close_upvalues(0),
                Instr::nop(),
                Instr::ret(crate::RetCount::Fixed(0)),
            ],
            line_info: vec![1, 2, 3],
            ..Bytecode::default()
        };
        strip_dead_instructions(&mut close_kept).expect("nop strips beside a close");
        assert_eq!(close_kept.code[0], Instr::close_upvalues(0));
        assert_eq!(close_kept.line_info, vec![1, 3]);
    }

    #[test]
    fn finalize_rejects_shared_nested_chunks_and_missing_lines() {
        let mut shared =
            parser::parse_unfinalized("return function() end").expect("nested fixture compiles");
        let retained_child = Arc::clone(&shared.nested[0]);
        let error = finalize(&mut shared).expect_err("shared child cannot be finalized in place");
        assert!(matches!(error.kind(), ErrorKind::InternalError(_)));
        drop(retained_child);

        let mut missing = parser::parse_unfinalized("return 1").expect("fixture compiles");
        missing.line_info.clear();
        let error = finalize(&mut missing).expect_err("missing lines must be recoverable");
        assert!(matches!(error.kind(), ErrorKind::InternalError(_)));
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
        let raw = parser::parse_unfinalized("f(1); obj:m(2)").expect("raw parser output compiles");
        assert_eq!(
            raw.code
                .iter()
                .filter(|inst| inst.opcode() == Instr::OP_NOP)
                .count(),
            2
        );
        let finalized = compile("f(1); obj:m(2)", None).expect("finalized output compiles");
        assert_no_opcode_in_tree(&finalized, Instr::OP_NOP);

        for source in ["local function f(...) end; f(...)", "f(g())"] {
            let dynamic = parser::parse_unfinalized(source).expect("raw dynamic call compiles");
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
    fn close_upvalues_kept_only_for_scopes_with_captures() {
        // `captures` also makes a closure, so a per-chunk rule could not drop
        // its uncaptured loop's closes; the per-scope capture log does.
        let finalized = compile(
            "local function plain() do local x = 1 end end; local function captures() for i = 1, 2 do local s = i end; for i = 1, 2 do if i then break end end; do local x = 1; f = function() return x end end end",
            None,
        )
        .expect("finalized nested functions compile");
        assert_no_opcode_in_tree(&finalized.nested[0], Instr::OP_CLOSE_UPVALUES);
        let closes: Vec<Instr> = finalized.nested[1]
            .code
            .iter()
            .copied()
            .filter(|inst| inst.opcode() == Instr::OP_CLOSE_UPVALUES)
            .collect();
        assert_eq!(closes, vec![Instr::close_upvalues(0)]);
    }

    #[test]
    fn break_close_is_decided_at_loop_exit() {
        // The break precedes the loop's only capture textually, so deciding at
        // break emission would drop it; deciding at loop exit keeps it.
        let raw = parser::parse_unfinalized(
            "while true do local z = 1; if z then break end; f = function() return z end end",
        )
        .expect("break fixture compiles");
        let closes: Vec<usize> = raw
            .code
            .iter()
            .enumerate()
            .filter(|(_, inst)| inst.opcode() == Instr::OP_CLOSE_UPVALUES)
            .map(|(index, _)| index)
            .collect();
        let first_closure = raw
            .code
            .iter()
            .position(|inst| inst.opcode() == Instr::OP_CLOSURE)
            .expect("fixture makes a closure");
        assert!(
            closes.iter().any(|&index| index < first_closure),
            "break close before the capture must be retained"
        );
    }

    #[test]
    fn stripping_preserves_costs_and_decreases_only_instruction_counts() {
        // The uncaptured loop's break close is NOP'd by the parser.
        let mut raw = parser::parse_unfinalized(
            "local x = 0; for i = 1, 3 do if i == 2 then break end; x = x + i end",
        )
        .expect("raw cost fixture compiles");
        let before = crate::ScopeCost::analyze_chunk(&raw, "main".to_string());
        finalize(&mut raw).expect("cost fixture finalizes");
        let after = crate::ScopeCost::analyze_chunk(&raw, "main".to_string());
        assert_eq!(after.own_cost, before.own_cost);
        assert_eq!(after.total_cost, before.total_cost);
        assert_eq!(after.arithmetic_ops(), before.arithmetic_ops());
        assert_eq!(after.table_creations(), before.table_creations());
        assert_eq!(after.table_writes(), before.table_writes());
        assert!(after.instructions < before.instructions);
    }
}
