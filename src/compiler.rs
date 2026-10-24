//! Functions and types associated with converting source code into bytecode.

#![cfg_attr(
    not(test),
    deny(clippy::panic, clippy::unreachable, clippy::expect_used)
)]

mod ast;
mod codegen;
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
    /// Capture a local variable (its register) from the immediately
    /// enclosing function.
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
    /// Number of slots in this function's field lookup cache (`OP_GET_FIELD`
    /// and `OP_SELF`).
    pub(crate) field_cache_slots: u8,
    /// Number of slots in this function's set-field lookup cache
    /// (`OP_SET_FIELD`).
    pub(crate) set_field_cache_slots: u8,
    /// Number of `pairs` cursor slots (`OP_TFOR_CALL`).
    pub(crate) tfor_cursor_slots: u8,
    pub(crate) num_params: u8,
    /// The most non-parameter locals the function has in scope at once.
    pub(crate) num_locals: u8,
    /// The registers a frame of this function reserves: parameters, locals
    /// and temporaries.
    pub(crate) max_stack: u16,
    pub(crate) nested: Vec<Arc<Bytecode>>,
    /// Describes the upvalues this function captures.
    pub(crate) upvalues: Vec<UpvalueDesc>,
    /// Whether this function accepts varargs (...).
    pub(crate) is_vararg: bool,
    /// Optional function name (for debugging/analysis), shared with the
    /// trace frames and VM faults that report it.
    pub(crate) name: Option<Arc<str>>,
    /// Source line where this function was defined, for tracebacks of
    /// unnamed functions. Zero marks a main chunk (lines are 1-based). Saved
    /// in snapshots, so tracebacks match before and after a restore.
    pub(crate) defined_line: u32,
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
    /// the pass is idempotent.
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
            let slot = match inst.opcode() {
                // An out-of-range literal id has no slot entry; it stays
                // uncached and the verifier rejects it right after.
                Instr::OP_GET_GLOBAL => global_slots
                    .get_mut(usize::from(inst.b()))
                    .and_then(|entry| *entry.get_or_insert_with(|| allocate(&mut global_len))),
                // Each SET_GLOBAL site takes a fresh slot of the shared global
                // cache, never one a GET_GLOBAL name owns.
                Instr::OP_SET_GLOBAL => allocate(&mut global_len),
                Instr::OP_GET_FIELD | Instr::OP_SELF => allocate(&mut field_len),
                Instr::OP_SET_FIELD => allocate(&mut set_field_len),
                Instr::OP_TFOR_CALL => allocate(&mut tfor_cursor_len),
                _ => continue,
            };
            *inst = inst.with_cache_slot(slot);
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

/// The one compile path: Lua source bytes plus an optional, byte-exact chunk
/// name, to finalized bytecode. The name is bounded before anything else is
/// examined, so an oversized name wins over a malformed or oversized source.
#[cfg_attr(feature = "hotpath", hotpath::measure)]
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
    bc.assign_cache_slots();
    // Runs in release too, and the result is propagated rather than dropped.
    // Verifying our own fresh output guards against a compiler bug, and the
    // only reason to pay for it is to act on it: a debug-only assertion would
    // let a release build emit malformed bytecode and discover it later as a
    // VM panic, which is precisely the failure mode this crate refuses.
    // Reporting it as a compile error keeps a compiler bug catchable instead
    // of fatal.
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
mod tests {
    use super::*;
    use crate::error::{ErrorKind, SyntaxError};
    use crate::limits::{MAX_SYNTAX_DEPTH, SYNTAX_SUFFIX_COST};

    /// The open-result shapes the verifier's producer and consumer rules exist
    /// for. If the corpus stops containing one of these, the acceptance proof
    /// silently stops covering them, so the corpus test pins that each was
    /// actually seen.
    #[derive(Default)]
    struct OpenShapes {
        open_call_args: bool,
        open_call_results: bool,
        open_set_list: bool,
        return_all: bool,
        vararg_all: bool,
    }

    fn assert_verified_tree(bytecode: &Bytecode, seen: &mut OpenShapes) {
        verify::validate_bytecode(bytecode)
            .expect("compiler output must pass bytecode verification");
        for instruction in &bytecode.code {
            match instruction.opcode() {
                Instr::OP_CALL => {
                    if instruction.call_args() == crate::ArgCount::Dynamic {
                        seen.open_call_args = true;
                    }
                    if instruction.call_results() == crate::RetCount::All {
                        seen.open_call_results = true;
                    }
                }
                Instr::OP_SET_LIST if instruction.set_list_count().is_none() => {
                    seen.open_set_list = true;
                }
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

    fn deepest_accepted_program(source_for_depth: impl Fn(usize) -> String) -> (usize, Bytecode) {
        deepest_accepted_program_until(source_for_depth, |kind| {
            matches!(
                kind,
                ErrorKind::SyntaxError(SyntaxError::TooManySyntaxLevels)
            )
        })
    }

    fn deepest_accepted_program_until(
        source_for_depth: impl Fn(usize) -> String,
        is_expected_limit: impl Fn(&ErrorKind) -> bool,
    ) -> (usize, Bytecode) {
        let mut deepest = None;
        let mut first_rejected = None;
        for depth in 1..=(MAX_SYNTAX_DEPTH as usize + 1) {
            let source = source_for_depth(depth);
            match compile(&source, None) {
                Ok(bytecode) => deepest = Some((depth, bytecode)),
                Err(error) => {
                    assert!(is_expected_limit(error.kind()), "{source}: {error}");
                    first_rejected = Some(depth);
                    break;
                }
            }
        }

        let (depth, bytecode) = deepest.expect("a simple nested program must compile");
        assert_eq!(first_rejected, Some(depth + 1));
        (depth, bytecode)
    }

    fn deepest_shape_compiles(
        source_for_depth: impl Fn(usize) -> String,
        is_expected_limit: impl Fn(&ErrorKind) -> bool,
    ) -> usize {
        let (depth, bytecode) = deepest_accepted_program_until(source_for_depth, is_expected_limit);
        verify::validate_bytecode(&bytecode).expect("deep compiler output verifies");
        depth
    }

    #[test]
    fn deepest_nested_programs_compile_within_the_test_stack() {
        std::thread::Builder::new()
            .stack_size(parser::DEEP_PARSE_TEST_STACK_BYTES)
            .spawn(deepest_nested_programs_body)
            .expect("spawning the nesting test thread should succeed")
            .join()
            .expect("nesting test thread should not panic");
    }

    #[test]
    fn nested_statement_and_expression_shapes_compile_within_the_test_stack() {
        std::thread::Builder::new()
            .stack_size(parser::DEEP_PARSE_TEST_STACK_BYTES)
            .spawn(nested_statement_and_expression_shapes_body)
            .expect("spawning the nesting test thread should succeed")
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload));
    }

    fn nested_statement_and_expression_shapes_body() {
        let syntax_limit = |kind: &ErrorKind| {
            matches!(
                kind,
                ErrorKind::SyntaxError(SyntaxError::TooManySyntaxLevels)
            )
        };

        for (open, close) in [
            ("do ", "end "),
            ("while false do ", "end "),
            ("repeat ", "until true "),
            ("if true then ", "end "),
        ] {
            deepest_shape_compiles(
                |depth| format!("{}return 1 {}", open.repeat(depth), close.repeat(depth)),
                syntax_limit,
            );
        }

        deepest_shape_compiles(
            |depth| {
                format!(
                    "{}return 1 {}",
                    "function f() ".repeat(depth),
                    "end ".repeat(depth)
                )
            },
            syntax_limit,
        );

        // Nested loops retain their hidden control locals, so MAX_LOCALS
        // rejects them before the syntax budget; still compile the deepest
        // admitted numeric and generic loop trees through codegen.
        let locals_limit =
            |kind: &ErrorKind| matches!(kind, ErrorKind::SyntaxError(SyntaxError::TooManyLocals));
        deepest_shape_compiles(
            |depth| {
                format!(
                    "{}return 1 {}",
                    "for i = 1, 1 do ".repeat(depth),
                    "end ".repeat(depth)
                )
            },
            locals_limit,
        );
        deepest_shape_compiles(
            |depth| {
                format!(
                    "{}return 1 {}",
                    "for key in iterator do ".repeat(depth),
                    "end ".repeat(depth)
                )
            },
            locals_limit,
        );

        deepest_shape_compiles(
            |depth| format!("return {}value", "- ".repeat(depth)),
            syntax_limit,
        );
        deepest_shape_compiles(
            |depth| format!("return {}value", "not ".repeat(depth)),
            syntax_limit,
        );
        deepest_shape_compiles(
            |depth| format!("return {}value", "value^".repeat(depth)),
            syntax_limit,
        );
        // Nested non-multi parentheses are normalized by the parser, and a
        // parenthesized call retains only one Paren node; this exercises the
        // accepted source depth without implying a recursive codegen chain.
        deepest_shape_compiles(
            |depth| format!("return {}value(){}", "(".repeat(depth), ")".repeat(depth)),
            syntax_limit,
        );
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
        verify::validate_bytecode(&call_chain).expect("deep call chain verifies");

        let (_, table_chain) = deepest_accepted_program(|depth| {
            let mut expression = "{g()}".to_string();
            for _ in 1..depth {
                expression = format!("{{f({expression})}}");
            }
            format!("return {expression}")
        });
        verify::validate_bytecode(&table_chain).expect("deep constructor chain verifies");
    }

    #[test]
    fn long_flat_chains_compile_without_deep_recursion() {
        // Operator, suffix and elseif chains are flat in the tree, so code
        // generation walks them iteratively however long they are.
        std::thread::Builder::new()
            .stack_size(parser::DEEP_PARSE_TEST_STACK_BYTES)
            .spawn(|| {
                let sum = format!("local x = 1; return x{}", "+x".repeat(20_000));
                compile(&sum, None).expect("long addition chain compiles");
                let fields = format!("local t = {{}}; return t{}", ".a".repeat(20_000));
                compile(&fields, None).expect("long field chain compiles");
                let arms = format!(
                    "local x = 1; if x == 0 then x = 0 {} end",
                    "elseif x == 1 then x = 1 ".repeat(5_000)
                );
                compile(&arms, None).expect("long elseif chain compiles");
            })
            .expect("spawning the chain test thread should succeed")
            .join()
            .expect("chain test thread should not panic");
    }

    #[test]
    fn compiler_examples_verify_and_disassemble() {
        let examples = crate::example_list::EXAMPLES;
        // Without this the corpus proof passes vacuously if the list is
        // emptied, which is exactly the failure mode that would let a
        // verifier that rejects real compiler output reach a release.
        assert!(
            examples.len() > 20,
            "the example corpus is the proof that the verifier accepts real \
             compiler output; found only {} sources",
            examples.len()
        );
        let mut seen = OpenShapes::default();
        for example in examples {
            let source = std::fs::read(example.path).expect("example source must be readable");
            let relative_path = example.relative;
            let bytecode = compile(&source, Some(relative_path.as_bytes()))
                .expect("example source must compile");
            assert_verified_tree(&bytecode, &mut seen);

            let text = crate::Disassembly::new(&bytecode).to_string();
            assert!(text.starts_with("function main "), "{relative_path}");
            for line in text.lines() {
                assert!(
                    line.is_empty()
                        || line.starts_with("function main ")
                        || line.starts_with("function main/")
                        || line.starts_with("  "),
                    "{relative_path}: malformed listing line {line:?}"
                );
                assert!(
                    !line.contains(crate::disassemble::OUT_OF_RANGE_ANNOTATION),
                    "{relative_path}: {line:?}"
                );
            }
        }
        assert!(
            seen.open_call_args,
            "corpus lost a call with open arguments"
        );
        assert!(
            seen.open_call_results,
            "corpus lost a call with all results"
        );
        assert!(seen.open_set_list, "corpus lost an open OP_SET_LIST");
        assert!(seen.return_all, "corpus lost OP_RETURN with RetCount::All");
        assert!(seen.vararg_all, "corpus lost OP_VARARG with all varargs");
    }

    fn opcodes(bc: &Bytecode, opcode: u8) -> Vec<Instr> {
        bc.code
            .iter()
            .copied()
            .filter(|inst| inst.opcode() == opcode)
            .collect()
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

        let get_globals = opcodes(&bc, Instr::OP_GET_GLOBAL);
        assert_eq!(get_globals.len(), 3);
        let set_globals = opcodes(&bc, Instr::OP_SET_GLOBAL);
        assert_eq!(set_globals.len(), 2);
        // 2 distinct read names (shared by literal) + 2 fresh SET sites; the
        // two non-global literals ("not a global", "field") hold no slot -
        // they would push this to 6 if literal count drove allocation.
        assert_eq!(bc.global_cache_slots, 4);
        assert_eq!(get_globals[0].cache_slot(), Some(0));
        assert_eq!(get_globals[1].cache_slot(), Some(0));
        assert_eq!(get_globals[2].cache_slot(), Some(2));
        assert_eq!(set_globals[0].cache_slot(), Some(1));
        assert_eq!(set_globals[1].cache_slot(), Some(3));
    }

    #[test]
    fn global_set_cache_uses_biased_fresh_slots_and_excludes_builtins() {
        let bc = compile("foo = 1; foo = 2; table = {}", None).expect("source compiles");
        let set_globals = opcodes(&bc, Instr::OP_SET_GLOBAL);

        assert_eq!(bc.global_cache_slots, 2);
        assert_eq!(set_globals.len(), 2);
        assert_eq!(set_globals[0].cache_slot(), Some(0));
        assert_eq!(set_globals[1].cache_slot(), Some(1));
        assert!(!opcodes(&bc, Instr::OP_SET_BUILTIN).is_empty());
    }

    #[test]
    fn global_set_cache_leaves_the_256th_site_uncached() {
        let source = (0..256)
            .map(|idx| format!("set_cache_{idx} = {idx}"))
            .collect::<Vec<_>>()
            .join("; ");
        let bc = compile(source, None).expect("source compiles");
        let set_globals = opcodes(&bc, Instr::OP_SET_GLOBAL);

        assert_eq!(usize::from(bc.global_cache_slots), MAX_CACHE_SLOTS);
        assert_eq!(set_globals.len(), 256);
        for (idx, inst) in set_globals.iter().take(255).enumerate() {
            assert_eq!(inst.cache_slot(), Some(idx as u8));
        }
        assert_eq!(set_globals[255].cache_slot(), None);
    }

    #[test]
    fn field_lookup_cache_tracks_get_field_and_method_sites() {
        let bc = compile(
            r#"
            local t = { x = 1, y = 2 }
            local s = t.x + t.x + t.y
            t:m()
            "#,
            None,
        )
        .unwrap();

        let get_fields = opcodes(&bc, Instr::OP_GET_FIELD);
        let selves = opcodes(&bc, Instr::OP_SELF);
        assert_eq!(get_fields.len(), 3);
        assert_eq!(selves.len(), 1);
        assert_eq!(bc.field_cache_slots, 4);
        for (slot, inst) in get_fields.iter().enumerate() {
            assert_eq!(inst.cache_slot(), Some(slot as u8));
        }
        assert_eq!(selves[0].cache_slot(), Some(3));
    }

    #[test]
    fn set_field_lookup_cache_tracks_set_field_sites() {
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

        let set_fields = opcodes(&bc, Instr::OP_SET_FIELD);
        assert_eq!(set_fields.len(), 3);
        assert_eq!(bc.set_field_cache_slots, 3);
        for (slot, inst) in set_fields.iter().enumerate() {
            assert_eq!(inst.cache_slot(), Some(slot as u8));
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

    fn assert_no_opcode_in_tree(bytecode: &Bytecode, opcode: u8) {
        assert!(bytecode.code.iter().all(|inst| inst.opcode() != opcode));
        for nested in &bytecode.nested {
            assert_no_opcode_in_tree(nested, opcode);
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
        assert_no_opcode_in_tree(&finalized.nested[0], Instr::OP_CLOSE);
        let closes = opcodes(&finalized.nested[1], Instr::OP_CLOSE);
        assert_eq!(closes, vec![Instr::close(0)]);
    }

    #[test]
    fn break_close_is_decided_at_loop_exit() {
        // The break precedes the loop's only capture textually, so deciding at
        // break emission would drop it; deciding at loop exit keeps it.
        let bc = compile(
            "while true do local z = 1; if z then break end; f = function() return z end end",
            None,
        )
        .expect("break fixture compiles");
        let closes: Vec<usize> = bc
            .code
            .iter()
            .enumerate()
            .filter(|(_, inst)| inst.opcode() == Instr::OP_CLOSE)
            .map(|(index, _)| index)
            .collect();
        let first_closure = bc
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
    fn frames_reserve_parameters_locals_and_temporaries() {
        let bc = compile("local a, b = 1, 2; return a + b * (a - b)", None).expect("compiles");
        // Two locals plus at least one temporary for the nested operands.
        assert!(usize::from(bc.max_stack) >= 3);
        let empty = compile("return function(x, y) end", None).expect("compiles");
        assert_eq!(empty.nested[0].max_stack, 2);
    }
}
