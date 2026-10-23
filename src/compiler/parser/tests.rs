use super::Bytecode;
use super::DEEP_PARSE_TEST_STACK_BYTES;
use super::Parser;
use super::TokenStream;
use super::parse_unfinalized;
use crate::State;
use crate::compiler::UpvalueDesc;
use crate::error::{ErrorKind, SyntaxError};
use crate::instr::{ArgCount, ArithOp, ArithOperands, CompareOp, Instr, RetCount};
use crate::limits::{
    MAX_ASSIGNMENT_OFFSET, MAX_ASSIGNMENT_TARGETS, MAX_CHUNK_NESTING, MAX_EXPRESSIONS,
    MAX_FIXED_ARGS, MAX_FIXED_RESULTS, MAX_LITERALS, MAX_LOCALS, MAX_NESTED, MAX_SOURCE_BYTES,
    MAX_TABLE_BATCH_SIZE, MAX_TABLE_TEMPLATE_FIELDS, MAX_TABLE_TEMPLATES, SYNTAX_SUFFIX_COST,
};

fn assert_line_info_matches_code_len(chunk: &Bytecode) {
    assert_eq!(chunk.code.len(), chunk.line_info.len());
    for nested in &chunk.nested {
        assert_line_info_matches_code_len(nested);
    }
}

fn assert_too_many_syntax_levels(source: &str) {
    let err = parse_unfinalized(source).expect_err("source must exceed the syntax depth limit");
    assert!(
        matches!(
            err.kind(),
            ErrorKind::SyntaxError(SyntaxError::TooManySyntaxLevels),
        ),
        "unexpected error: {err:?}"
    );
}

/// The opcodes of every arithmetic form of `op`.
fn arith_opcodes(op: ArithOp) -> [u8; 3] {
    [
        op.opcode(ArithOperands::RegReg),
        op.opcode(ArithOperands::RegConst),
        op.opcode(ArithOperands::ConstReg),
    ]
}

fn has_arith(chunk: &Bytecode, op: ArithOp) -> bool {
    chunk
        .code
        .iter()
        .any(|inst| arith_opcodes(op).contains(&inst.opcode()))
}

fn opcodes(chunk: &Bytecode, opcode: u8) -> Vec<Instr> {
    chunk
        .code
        .iter()
        .copied()
        .filter(|inst| inst.opcode() == opcode)
        .collect()
}

fn run(source: &str) -> State {
    let mut state = State::new();
    crate::Program::compile(source, None)
        .and_then(|program| state.load(&program))
        .expect("fixture compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("fixture runs");
    state
}

fn eval_number(source: &str) -> f64 {
    run(source).to_number(-1).expect("fixture returns a number")
}

#[test]
fn break_outside_loop_uses_the_source_line() {
    let err = parse_unfinalized("\n\nbreak").expect_err("break outside a loop must fail");

    assert!(matches!(
        err.kind(),
        ErrorKind::SyntaxError(SyntaxError::BreakOutsideLoop)
    ));
    assert_eq!(err.line_num(), 3);
    assert_eq!(err.to_string(), "3:6: <break> not inside a loop");
}

#[test]
fn decimal_and_hex_literals_use_the_shared_numeral_parser() {
    for source in ["42", ".5", "1.", "1.e2", "1e-3", "0x1.8p+0"] {
        let expected = crate::numeral::parse_lua_numeral(source.as_bytes())
            .expect("literal fixture must be a valid numeral");
        let chunk =
            parse_unfinalized(&format!("return {source}")).expect("literal fixture must compile");
        let actual = chunk
            .number_literals
            .first()
            .expect("literal must be stored in the number pool");

        assert_eq!(actual.to_bits(), expected.to_bits(), "{source}");
    }
}

fn literal_bytes(source: &str) -> Vec<u8> {
    let chunk = parse_unfinalized(source).expect("test string literal should compile");
    chunk
        .string_literals
        .into_iter()
        .next()
        .expect("test string literal should be present")
}

#[test]
fn literal_string_decodes_decimal_and_hex_escapes_as_bytes() {
    for (source, expected) in [
        (r#"return "\0""#, vec![0]),
        (r#"return "\1""#, vec![1]),
        (r#"return "\12""#, vec![12]),
        (r#"return "\065""#, vec![65]),
        (r#"return "\1234""#, vec![123, b'4']),
        (r#"return "\255""#, vec![255]),
        (r#"return "\x00""#, vec![0]),
        (r#"return "\x41""#, vec![65]),
        (r#"return "\xFF""#, vec![255]),
    ] {
        assert_eq!(literal_bytes(source), expected, "{source}");
    }
}

#[test]
fn literal_string_decodes_z_and_named_escapes() {
    assert_eq!(literal_bytes("return \"a\\z \t\n b\""), b"ab");
    assert_eq!(
        literal_bytes(r#"return "\n\t\r\\\"\'\a\b\f\v""#),
        vec![b'\n', b'\t', b'\r', b'\\', b'\"', b'\'', 7, 8, 12, 11]
    );
    assert_eq!(literal_bytes("return \"a\\\nb\""), b"a\nb");
}

#[test]
fn literal_string_normalizes_escaped_physical_newlines() {
    for source in [
        "return \"a\\\rb\"",
        "return \"a\\\r\nb\"",
        "return \"a\\\n\rb\"",
    ] {
        assert!(source.as_bytes().contains(&b'\r'));
        assert_eq!(literal_bytes(source), b"a\nb", "{source:?}");
    }
}

#[test]
fn literal_string_z_skips_vertical_tab() {
    let source = "return \"a\\z\x0b b\"";
    assert!(source.as_bytes().contains(&0x0b));
    assert_eq!(literal_bytes(source), b"ab");
}

#[test]
fn literal_string_escape_errors_use_the_backslash_position() {
    for (source, expected) in [
        (r#"return "\256""#, "decimal escape"),
        (r#"return "\999""#, "decimal escape"),
        (r#"return "\x""#, "hexadecimal escape"),
        (r#"return "\x4""#, "hexadecimal escape"),
        (r#"return "\x4G""#, "hexadecimal escape"),
        (r#"return "\q""#, "invalid escape"),
    ] {
        let err = parse_unfinalized(source).expect_err("malformed escape must fail");
        assert!(
            matches!(
                (err.kind(), expected),
                (
                    ErrorKind::SyntaxError(SyntaxError::DecimalEscapeTooLarge),
                    "decimal escape"
                ) | (
                    ErrorKind::SyntaxError(SyntaxError::HexadecimalDigitExpected),
                    "hexadecimal escape"
                ) | (
                    ErrorKind::SyntaxError(SyntaxError::InvalidEscapeSequence),
                    "invalid escape"
                )
            ),
            "unexpected error: {err}"
        );
        assert_eq!(
            err.column() as usize,
            source.find('\\').expect("source has escape") + 1
        );
    }
}

// ---------------------------------------------------------------------------
// Register shapes
// ---------------------------------------------------------------------------

#[test]
fn local_arithmetic_reads_and_writes_registers_in_place() {
    let chunk = parse_unfinalized("local a, b, c = 1, 2, 3; c = a + b").expect("compiles");
    let add = opcodes(&chunk, ArithOp::Add.opcode(ArithOperands::RegReg));
    assert_eq!(
        add,
        vec![Instr::arith(ArithOp::Add, ArithOperands::RegReg, 2, 0, 1)]
    );
    assert!(opcodes(&chunk, Instr::OP_MOVE).is_empty());
}

#[test]
fn number_literal_operands_use_constant_forms() {
    let chunk =
        parse_unfinalized("local a = 1; local b = a + 2; local c = 3 - a").expect("compiles");
    assert_eq!(
        opcodes(&chunk, ArithOp::Add.opcode(ArithOperands::RegConst)).len(),
        1
    );
    assert_eq!(
        opcodes(&chunk, ArithOp::Sub.opcode(ArithOperands::ConstReg)).len(),
        1
    );
}

#[test]
fn method_calls_look_up_with_self_and_mark_the_call() {
    let chunk = parse_unfinalized("local o = {}; o:m(1)").expect("compiles");
    assert_eq!(opcodes(&chunk, Instr::OP_SELF).len(), 1);
    let calls = opcodes(&chunk, Instr::OP_CALL);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].call_is_method());
    assert_eq!(calls[0].call_args(), ArgCount::Fixed(2));
}

#[test]
fn conditions_ending_in_a_comparison_branch_in_one_instruction() {
    for (source, op) in [
        ("local n = 0 while n < 3 do n = n + 1 end", CompareOp::Lt),
        (
            "local a = 1 if a >= 2 then return 1 end return 0",
            CompareOp::Ge,
        ),
        ("local n = 0 repeat n = n + 1 until n ~= 1", CompareOp::Ne),
    ] {
        let chunk = parse_unfinalized(source).expect("compiles");
        assert_eq!(opcodes(&chunk, op.branch_opcode()).len(), 1, "{source}");
        assert!(opcodes(&chunk, op.value_opcode()).is_empty(), "{source}");
    }
}

#[test]
fn open_results_are_consumed_by_the_next_instruction() {
    for source in [
        "local function f(...) return ... end; return f(f(f(1, 2, 3)))",
        "local t = {}; function t:f(...) return ... end; return t:f(t:f(t:f(1, 2, 3)))",
        "local t = {7, g()}",
        "local function f(...) local t = {99, ...} end",
        "return g()",
        "print(type(nil))",
    ] {
        let chunk = crate::compiler::compile(source, None).expect("compiles");
        let check = |chunk: &Bytecode| {
            for (pc, inst) in chunk.code.iter().enumerate() {
                if inst.produces_open() {
                    assert!(chunk.code[pc + 1].consumes_open(), "{source} at {pc}");
                }
            }
        };
        check(&chunk);
        for nested in &chunk.nested {
            check(nested);
        }
    }
}

#[test]
fn table_constructor_expands_only_final_bare_call_or_vararg() {
    let final_call = parse_unfinalized("y = {7, f()}").unwrap();
    assert!(
        final_call
            .code
            .iter()
            .any(|i| i.opcode() == Instr::OP_CALL && i.call_results() == RetCount::All)
    );
    assert!(
        opcodes(&final_call, Instr::OP_SET_LIST)
            .iter()
            .any(|i| i.set_list_count().is_none())
    );

    let non_final_call = parse_unfinalized("y = {f(), 7}").unwrap();
    assert!(
        non_final_call
            .code
            .iter()
            .any(|i| i.opcode() == Instr::OP_CALL && i.call_results() == RetCount::Fixed(1))
    );
    assert_eq!(
        opcodes(&non_final_call, Instr::OP_SET_LIST)[0].set_list_count(),
        Some(2)
    );

    let final_vararg = parse_unfinalized("y = {99, ...}").unwrap();
    assert!(
        final_vararg
            .code
            .iter()
            .any(|i| i.opcode() == Instr::OP_VARARG && i.vararg_count() == RetCount::All)
    );

    let non_final_vararg = parse_unfinalized("y = {..., 99}").unwrap();
    assert!(
        non_final_vararg
            .code
            .iter()
            .any(|i| i.opcode() == Instr::OP_VARARG && i.vararg_count() == RetCount::Fixed(1))
    );
    assert_eq!(
        opcodes(&non_final_vararg, Instr::OP_SET_LIST)[0].set_list_count(),
        Some(2)
    );
}

#[test]
fn table_constructor_sizing_follows_its_keyed_fields() {
    // A keyed final field stops the expansion; a call before it is fixed.
    let keyed_last = parse_unfinalized("y = {f(), k = 7}").unwrap();
    assert_eq!(opcodes(&keyed_last, Instr::OP_NEW_TABLE).len(), 1);
    assert_eq!(
        opcodes(&keyed_last, Instr::OP_SET_LIST)[0].set_list_count(),
        Some(1)
    );

    // An open constructor's hint counts named and computed keys only.
    let capacity = parse_unfinalized("y = {1, 2, a = 1, [k] = 2, f()}").unwrap();
    assert_eq!(opcodes(&capacity, Instr::OP_NEW_TABLE_PRESIZED)[0].b(), 2);

    // Positional fields alone need no hash hint; SET_LIST reserves the array.
    let positional = parse_unfinalized("y = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10}").unwrap();
    assert_eq!(opcodes(&positional, Instr::OP_NEW_TABLE).len(), 1);
    assert!(opcodes(&positional, Instr::OP_NEW_TABLE_PRESIZED).is_empty());

    // Five or more unique named fields use a template; duplicates or computed
    // keys fall back to a presized table.
    let template = parse_unfinalized("y = {a = 1, b = 2, c = 3, d = 4, e = 5}").unwrap();
    assert_eq!(template.table_templates, vec![vec![1, 2, 3, 4, 5]]);
    assert_eq!(opcodes(&template, Instr::OP_INIT_FIELD_PINNED).len(), 5);
    for source in [
        "y = {a = 1, b = 2, c = 3, d = 4, a = 5}",
        "y = {a = 1, b = 2, c = 3, d = 4, [k] = 5}",
    ] {
        let chunk = parse_unfinalized(source).unwrap();
        assert!(chunk.table_templates.is_empty(), "{source}");
        assert_eq!(
            opcodes(&chunk, Instr::OP_NEW_TABLE_PRESIZED)[0].b(),
            5,
            "{source}"
        );
    }
}

#[test]
fn table_constructor_does_not_expand_non_bare_tails() {
    for source in ["y = {(f())}", "y = {f().x}", "y = {(...)}"] {
        let chunk = parse_unfinalized(source).unwrap();
        assert!(chunk.code.iter().all(|i| !i.produces_open()), "{source}");
        assert_eq!(
            opcodes(&chunk, Instr::OP_SET_LIST)[0].set_list_count(),
            Some(1),
            "{source}"
        );
    }
}

#[test]
fn assignment_tail_vararg_expands_to_lvalue_count() {
    let chunk = parse_unfinalized("f = function(...) a, b = ... end").unwrap();
    let varargs = opcodes(&chunk.nested[0], Instr::OP_VARARG);
    assert_eq!(varargs.len(), 1);
    assert_eq!(varargs[0].vararg_count(), RetCount::Fixed(2));
}

#[test]
fn generic_for_tail_vararg_expands_to_iterator_triple() {
    let chunk = parse_unfinalized("f = function(...) for x in ... do end end").unwrap();
    let varargs = opcodes(&chunk.nested[0], Instr::OP_VARARG);
    assert_eq!(varargs[0].vararg_count(), RetCount::Fixed(3));
}

#[test]
fn non_final_vararg_stays_single_valued() {
    let chunk = parse_unfinalized("f = function(...) a, b, c = ..., 99 end").unwrap();
    let varargs = opcodes(&chunk.nested[0], Instr::OP_VARARG);
    assert_eq!(varargs[0].vararg_count(), RetCount::Fixed(1));
}

#[test]
fn local_declarations_land_in_their_own_registers() {
    let chunk = parse_unfinalized("f = function(...) local a, b, c = ... end").unwrap();
    let varargs = opcodes(&chunk.nested[0], Instr::OP_VARARG);
    assert_eq!(varargs, vec![Instr::vararg(0, RetCount::Fixed(3))]);

    let chunk = parse_unfinalized("f = function() local a, b, c = g() end").unwrap();
    let calls = opcodes(&chunk.nested[0], Instr::OP_CALL);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].a(), 0);
    assert_eq!(calls[0].call_results(), RetCount::Fixed(3));
}

#[test]
fn parameterized_function_tracks_only_non_parameter_locals() {
    let chunk = parse_unfinalized("function f(a, b) do local x end do local y end end").unwrap();
    let function = &chunk.nested[0];

    assert_eq!(function.num_params, 2);
    assert_eq!(function.num_locals, 1);
    assert!(function.max_stack >= 3);
}

#[test]
fn for_control_function_captures_enclosing_same_named_local() {
    let chunk = parse_unfinalized("local i = 10; for i = (function() return i end)(), 10 do end")
        .expect("for control fixture must compile");
    let function = &chunk.nested[0];

    assert_eq!(function.upvalues, vec![UpvalueDesc::Local(0)]);
}

// ---------------------------------------------------------------------------
// Evaluation order and binding preservation
// ---------------------------------------------------------------------------

#[test]
fn a_local_operand_keeps_the_value_it_had_when_evaluated() {
    // A closure writes the local while the right operand runs; the left
    // operand was already evaluated.
    assert_eq!(
        eval_number(
            "local x = 1 \
             local function change() x = 9 return 2 end \
             return x + change()"
        ),
        3.0
    );
    // The same through the current function's upvalue.
    assert_eq!(
        eval_number(
            "local x = 1 \
             local function change() x = 9 return 2 end \
             local function calculate() return x + change() end \
             return calculate()"
        ),
        3.0
    );
    // A capture that appears later in the source, reached on a later
    // iteration.
    assert_eq!(
        eval_number(
            "local x = 1 local h local total = 0 \
             for i = 1, 2 do \
               total = total + (x + (h and h() or 0)) \
               h = function() x = 100 return 0 end \
             end \
             return total"
        ),
        2.0
    );
    // A comparison operand and an index receiver.
    assert_eq!(
        eval_number(
            "local x = 1 local function change() x = 9 return 2 end \
             if x < change() then return 1 end return 0"
        ),
        1.0
    );
    assert_eq!(
        eval_number(
            "local t = {10} local function change() t = {20} return 1 end \
             return t[change()]"
        ),
        10.0
    );
    // A field store's receiver.
    assert_eq!(
        eval_number(
            "local t = {} local old = t local function change() t = {} return 5 end \
             t.x = change() return old.x"
        ),
        5.0
    );
}

#[test]
fn multiple_assignment_stores_see_the_operands_as_evaluated() {
    assert_eq!(
        eval_number(
            "local t = {} local i = 1 \
             t[i], i = 10, 2 \
             return (t[1] or 0) * 100 + (t[2] or 0)"
        ),
        1000.0
    );
    assert_eq!(
        eval_number("local t = {} local old = t t.x, t = 10, {} return old.x"),
        10.0
    );
    assert_eq!(
        eval_number("local a, b = 1, 2 a, b = b, a return a * 10 + b"),
        21.0
    );
}

#[test]
fn and_or_values_and_short_circuits() {
    assert_eq!(eval_number("local a = 5 return (a or 2) * 3"), 15.0);
    assert_eq!(eval_number("local a = false return (a or 2) * 3"), 6.0);
    assert_eq!(eval_number("local a = 5 return (a and 2) * 3"), 6.0);
    assert_eq!(eval_number("local x = 1 x = 2 or x return x"), 2.0);
    assert_eq!(eval_number("local x = 1 x = false or x return x"), 1.0);
    assert_eq!(
        eval_number("local a = false if a or 1 < 2 then return 1 end return 0"),
        1.0
    );
    assert_eq!(
        eval_number("local a = false if a or 2 < 1 then return 1 end return 0"),
        0.0
    );
}

#[test]
fn break_allows_semicolons_and_following_statements() {
    for source in [
        "while true do break; end",
        "while true do break;;; end",
        "while true do break; reached = 1 end",
    ] {
        parse_unfinalized(source).expect("break must allow following statements");
    }

    assert_eq!(
        eval_number("local reached = 0; while true do break; reached = 1 end; return reached"),
        0.0
    );
}

#[test]
fn a_new_line_paren_continues_the_call() {
    // A `(` on a new line continues the previous expression as a call, as in
    // reference Lua: the program is `print()(foo)()`.
    let expected = parse_unfinalized("print()(foo)()").expect("same-line form compiles");
    for text in [
        "print()\n(foo)()\n",
        "print()\r(foo)()\r",
        "print()\r\n(foo)()\r\n",
        "print()\n\r(foo)()\n\r",
    ] {
        let chunk = parse_unfinalized(text).unwrap_or_else(|e| panic!("{text:?}: {e:?}"));
        assert_eq!(chunk.code, expected.code, "{text:?}");
    }
}

#[test]
fn line_info_matches_code_len() {
    for source in [
        "print(1)",
        "local a, b = f()",
        "a, b, c = f()",
        "return f(1)",
        "local function f(...) return f(...), ... end",
        "t = {f()}",
        "local function f(...) t = {...} end",
        "obj:m(1)",
        "obj:m(f())",
    ] {
        let chunk = parse_unfinalized(source).expect("line-info fixture must compile");
        assert_line_info_matches_code_len(&chunk);
    }
}

#[test]
fn line_info_reports_correct_line() {
    let mut state = State::new();
    crate::Program::compile("function f() end\nf(); f(); f(); f(); f(); f(); f(); f(); f(); f()\nf(); f(); f(); f(); f(); f(); f(); f(); f(); f()\n\nlocal x = nil; x()", None).and_then(|program| state.load(&program))
        .expect("line-info fixture must compile");
    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("calling nil must fail");
    assert_eq!(
        err.stack_trace()
            .first()
            .expect("runtime error must carry a stack frame")
            .line,
        5
    );
}

#[test]
fn recursive_line_info_keeps_multiline_token_and_call_lines() {
    for newline in ["\n", "\r", "\r\n", "\n\r"] {
        let source = format!(
            "local function f(){newline}local s = \"a\\z{newline} b\"{newline}-- comment{newline}g(){newline}obj:m(){newline}end"
        );
        let chunk = parse_unfinalized(&source).expect("line-info fixture must compile");
        let function = &chunk.nested[0];

        assert!(
            function.line_info.iter().all(|line| (2..=7).contains(line)),
            "{newline:?}: {:?}",
            function.line_info
        );
        let lines_of = |opcode: u8| -> Vec<u32> {
            function
                .code
                .iter()
                .zip(&function.line_info)
                .filter_map(|(instruction, line)| (instruction.opcode() == opcode).then_some(*line))
                .collect()
        };
        assert_eq!(lines_of(Instr::OP_CALL), vec![5, 6], "{newline:?}");
        assert_eq!(lines_of(Instr::OP_SELF), vec![6], "{newline:?}");
        assert_eq!(lines_of(Instr::OP_RETURN), vec![7], "{newline:?}");
    }
}

#[test]
fn parser_error_positions_stay_pinned_after_comments_and_newlines() {
    for newline in ["\n", "\r", "\r\n", "\n\r"] {
        let after_comment = format!("-- comment{newline}end");
        let err = parse_unfinalized(&after_comment).expect_err("stray end must fail");
        assert!(matches!(
            err.kind(),
            ErrorKind::SyntaxError(SyntaxError::ExpectedToken { .. })
        ));
        assert_eq!((err.line_num(), err.column()), (2, 1), "{newline:?}");

        let bad_escape = format!("-- comment{newline}return \"\\q\"");
        let err = parse_unfinalized(&bad_escape).expect_err("bad escape must fail");
        assert!(matches!(
            err.kind(),
            ErrorKind::SyntaxError(SyntaxError::InvalidEscapeSequence)
        ));
        assert_eq!((err.line_num(), err.column()), (2, 9), "{newline:?}");

        let invalid_after_newline = format!("x = 1{newline}@");
        let err =
            parse_unfinalized(&invalid_after_newline).expect_err("invalid character must fail");
        assert!(matches!(
            err.kind(),
            ErrorKind::SyntaxError(SyntaxError::InvalidCharacter('@'))
        ));
        assert_eq!((err.line_num(), err.column()), (2, 2), "{newline:?}");
    }
}

#[test]
fn nested_chunks_preserve_shape_and_source_name() {
    let chunk = super::parse(
        b"function f() function g() end end",
        Some(std::sync::Arc::from(&b"nested-lines.lua"[..])),
    )
    .expect("nested fixture must compile");

    assert_eq!(chunk.source.as_deref(), Some(&b"nested-lines.lua"[..]));
    assert_eq!(
        chunk.code,
        vec![
            Instr::closure(0, 0),
            Instr::set_global(0, 0, None),
            Instr::ret(0, RetCount::Fixed(0)),
        ]
    );
    assert_eq!(chunk.string_literals, vec![Vec::from("f")]);

    let f = &chunk.nested[0];
    assert_eq!(f.source.as_deref(), Some(&b"nested-lines.lua"[..]));
    assert_eq!(f.string_literals, vec![Vec::from("g")]);

    let g = &f.nested[0];
    assert_eq!(g.source.as_deref(), Some(&b"nested-lines.lua"[..]));
    assert_eq!(g.code, vec![Instr::ret(0, RetCount::Fixed(0))]);
}

#[test]
fn named_source_is_attached_to_syntax_errors() {
    let error = super::parse(b"end", Some(std::sync::Arc::from(&b"named-error.lua"[..])))
        .expect_err("stray end must fail");

    assert_eq!(error.source_name(), Some(&b"named-error.lua"[..]));
    assert!(error.to_string().starts_with("named-error.lua:1:1:"));
}

fn upvalue_program(grandparent_locals: usize, parent_locals: usize) -> String {
    let grandparent = (0..grandparent_locals)
        .map(|i| format!("a{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let parent = (0..parent_locals)
        .map(|i| format!("b{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let references = (0..grandparent_locals)
        .map(|i| format!("a{i}"))
        .chain((0..parent_locals).map(|i| format!("b{i}")))
        .collect::<Vec<_>>()
        .join(" + ");
    format!(
        "local {grandparent}\nlocal function parent() local {parent}; return function() return {references} end end"
    )
}

fn identifier_list(prefix: &str, count: usize) -> String {
    (0..count)
        .map(|index| format!("{prefix}{index}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn value_list(count: usize) -> String {
    vec!["1"; count].join(", ")
}

fn named_table_fields(count: usize) -> String {
    (0..count)
        .map(|index| format!("k{index} = 1"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn parser_for_limit_test() -> Parser<'static> {
    Parser {
        input: TokenStream::new(b""),
        funcs: super::FunctionStack {
            frames: vec![super::state::FuncState::new(None, 0, false)],
        },
        source_name: None,
        syntax_depth: 0,
    }
}

#[test]
fn literal_pool_limits_match_their_error_messages() {
    let max_literal_count = u32::try_from(MAX_LITERALS).expect("MAX_LITERALS fits in u32");
    let mut parser = parser_for_limit_test();
    parser.funcs.chunk.number_literals = (0..max_literal_count - 1).map(f64::from).collect();
    parser.funcs.literal_indices.numbers = parser
        .funcs
        .chunk
        .number_literals
        .iter()
        .enumerate()
        .map(|(index, number)| {
            (
                super::number_literal_key(*number).expect("limit fixture numbers are not NaN"),
                u16::try_from(index).expect("literal test index fits in u16"),
            )
        })
        .collect();
    assert_eq!(
        parser
            .find_or_add_number(f64::from(max_literal_count - 1))
            .expect("MAX_LITERALS numbers must fit"),
        u16::MAX
    );
    let error = parser
        .find_or_add_number(f64::from(max_literal_count))
        .expect_err("one more than MAX_LITERALS must fail");
    assert!(matches!(
        error.kind(),
        ErrorKind::SyntaxError(SyntaxError::TooManyNumbers)
    ));
    assert!(error.to_string().contains(&format!("limit {MAX_LITERALS}")));

    let mut parser = parser_for_limit_test();
    parser.funcs.literal_indices.strings = (0..MAX_LITERALS - 1)
        .map(|index| index.to_string().into_bytes())
        .collect();
    assert_eq!(
        parser
            .find_or_add_string_bytes(b"last")
            .expect("MAX_LITERALS strings must fit"),
        u16::MAX
    );
    let error = parser
        .find_or_add_string_bytes(b"overflow")
        .expect_err("one more than MAX_LITERALS must fail");
    assert!(matches!(
        error.kind(),
        ErrorKind::SyntaxError(SyntaxError::TooManyStrings)
    ));
    assert!(error.to_string().contains(&format!("limit {MAX_LITERALS}")));
}

#[test]
fn source_size_limit_runs_before_lexing() {
    let mut source = String::with_capacity(MAX_SOURCE_BYTES + 1);
    source.push('@');
    source.push_str(&" ".repeat(MAX_SOURCE_BYTES));

    let error =
        parse_unfinalized(&source).expect_err("source one byte over the limit must be rejected");
    assert!(matches!(
        error.kind(),
        ErrorKind::SourceSizeExceeded {
            size,
            limit: MAX_SOURCE_BYTES,
        } if *size == MAX_SOURCE_BYTES + 1
    ));

    source.pop();
    let error =
        parse_unfinalized(&source).expect_err("the exact-size fixture must reach the lexer");
    assert!(matches!(
        error.kind(),
        ErrorKind::SyntaxError(SyntaxError::InvalidCharacter('@'))
    ));
}

#[test]
fn literal_pool_indexes_keep_first_insertion_order() {
    let chunk = parse_unfinalized("return 3, 1, 3, \"b\", \"a\", \"b\"")
        .expect("literal pool fixture must compile");

    assert_eq!(chunk.number_literals, vec![3.0, 1.0]);
    assert_eq!(chunk.string_literals, vec![b"b".to_vec(), b"a".to_vec()]);
}

#[test]
fn number_literal_index_matches_numeric_equality() {
    let mut parser = parser_for_limit_test();
    let positive_zero = parser
        .find_or_add_number(0.0)
        .expect("zero must fit in the number pool");
    let negative_zero = parser
        .find_or_add_number(-0.0)
        .expect("negative zero must fit in the number pool");
    let first_nan = parser
        .find_or_add_number(f64::NAN)
        .expect("NaN must fit in the number pool");
    let second_nan = parser
        .find_or_add_number(f64::NAN)
        .expect("a second NaN must fit in the number pool");

    assert_eq!(positive_zero, negative_zero);
    assert_ne!(first_nan, second_nan);
}

#[test]
fn local_limit_matches_its_error_message() {
    let source = format!("local {}", identifier_list("v", MAX_LOCALS));
    parse_unfinalized(&source).expect("MAX_LOCALS local variables must compile");

    let source = format!("local {}", identifier_list("v", MAX_LOCALS + 1));
    let error = parse_unfinalized(&source).expect_err("one more than MAX_LOCALS must fail");
    assert!(matches!(
        error.kind(),
        ErrorKind::SyntaxError(SyntaxError::TooManyLocals)
    ));
    assert!(error.to_string().contains(&format!("limit {MAX_LOCALS}")));
}

#[test]
fn fixed_argument_limit_matches_its_error_message() {
    let source = format!("f({})", value_list(MAX_FIXED_ARGS));
    parse_unfinalized(&source).expect("MAX_FIXED_ARGS arguments must compile");

    let source = format!("f({})", value_list(MAX_FIXED_ARGS + 1));
    let error = parse_unfinalized(&source).expect_err("one more than MAX_FIXED_ARGS must fail");
    assert!(matches!(
        error.kind(),
        ErrorKind::SyntaxError(SyntaxError::TooManyArguments)
    ));
    assert!(
        error
            .to_string()
            .contains(&format!("limit {MAX_FIXED_ARGS}"))
    );
}

#[test]
fn expression_limit_matches_its_error_message() {
    let source = format!("return {}", value_list(usize::from(MAX_EXPRESSIONS)));
    parse_unfinalized(&source).expect("MAX_EXPRESSIONS values must compile");

    let source = format!("return {}", value_list(usize::from(MAX_EXPRESSIONS) + 1));
    let error = parse_unfinalized(&source).expect_err("one more than MAX_EXPRESSIONS must fail");
    assert!(matches!(
        error.kind(),
        ErrorKind::SyntaxError(SyntaxError::TooManyExpressions)
    ));
    assert!(
        error
            .to_string()
            .contains(&format!("limit {MAX_EXPRESSIONS}"))
    );
}

#[test]
fn fixed_result_limit_matches_its_error_message() {
    let source = format!("{} = f()", identifier_list("v", MAX_FIXED_RESULTS));
    parse_unfinalized(&source).expect("MAX_FIXED_RESULTS values must compile");

    let source = format!("{} = f()", identifier_list("v", MAX_FIXED_RESULTS + 1));
    let error = parse_unfinalized(&source).expect_err("one more than MAX_FIXED_RESULTS must fail");
    assert!(matches!(
        error.kind(),
        ErrorKind::SyntaxError(SyntaxError::TooManyFixedResults)
    ));
    assert!(
        error
            .to_string()
            .contains(&format!("limit {MAX_FIXED_RESULTS}"))
    );
}

#[test]
fn field_targets_reach_the_assignment_target_cap() {
    // The last field target sits at offset `MAX_ASSIGNMENT_TARGETS - 1`, so
    // the target cap rejects a longer list before any receiver offset can
    // pass `MAX_ASSIGNMENT_OFFSET`.
    const _: () = assert!(MAX_ASSIGNMENT_TARGETS - 1 <= MAX_ASSIGNMENT_OFFSET);
    let targets = vec!["t.k"; MAX_ASSIGNMENT_TARGETS].join(", ");
    parse_unfinalized(&format!("{targets} = 1"))
        .expect("the maximum number of field targets must compile");

    let targets = vec!["t.k"; MAX_ASSIGNMENT_TARGETS + 1].join(", ");
    let error = parse_unfinalized(&format!("{targets} = 1"))
        .expect_err("a target past MAX_ASSIGNMENT_TARGETS must fail");
    assert!(matches!(
        error.kind(),
        ErrorKind::SyntaxError(SyntaxError::TooManyAssignmentTargets { count })
            if *count == MAX_ASSIGNMENT_TARGETS + 1
    ));
    assert!(
        error
            .to_string()
            .contains(&format!("limit {MAX_ASSIGNMENT_TARGETS}"))
    );
}

#[test]
fn nested_function_limit_matches_its_error_message() {
    let source = (0..MAX_NESTED)
        .map(|index| format!("function f{index}() end"))
        .collect::<Vec<_>>()
        .join("\n");
    parse_unfinalized(&source).expect("MAX_NESTED child functions must compile");

    let source = (0..MAX_NESTED + 1)
        .map(|index| format!("function f{index}() end"))
        .collect::<Vec<_>>()
        .join("\n");
    let error = parse_unfinalized(&source).expect_err("one more than MAX_NESTED must fail");
    assert!(matches!(
        error.kind(),
        ErrorKind::SyntaxError(SyntaxError::TooManyNestedFunctions)
    ));
    assert!(error.to_string().contains(&format!("limit {MAX_NESTED}")));
}

#[test]
fn table_template_field_limit_uses_the_generic_fallback() {
    let source = format!("t = {{{}}}", named_table_fields(MAX_TABLE_TEMPLATE_FIELDS));
    let bytecode = parse_unfinalized(&source).expect("MAX_TABLE_TEMPLATE_FIELDS fields must fit");
    assert_eq!(bytecode.table_templates[0].len(), MAX_TABLE_TEMPLATE_FIELDS);

    let source = format!(
        "t = {{{}}}",
        named_table_fields(MAX_TABLE_TEMPLATE_FIELDS + 1)
    );
    let bytecode =
        parse_unfinalized(&source).expect("one more template field must use generic stores");
    assert!(bytecode.table_templates.is_empty());
    assert!(!opcodes(&bytecode, Instr::OP_NEW_TABLE_PRESIZED).is_empty());
}

#[test]
fn table_template_count_limit_uses_the_generic_fallback() {
    let table = "t = {a = 1, b = 2, c = 3, d = 4, e = 5}";
    let source = vec![table; MAX_TABLE_TEMPLATES].join("\n");
    let bytecode = parse_unfinalized(&source).expect("MAX_TABLE_TEMPLATES constructors must fit");
    assert_eq!(bytecode.table_templates.len(), MAX_TABLE_TEMPLATES);

    let source = vec![table; MAX_TABLE_TEMPLATES + 1].join("\n");
    let bytecode = parse_unfinalized(&source).expect("extra templates must use generic stores");
    assert_eq!(bytecode.table_templates.len(), MAX_TABLE_TEMPLATES);
    assert!(!opcodes(&bytecode, Instr::OP_NEW_TABLE_PRESIZED).is_empty());
}

#[test]
fn table_array_batch_limit_splits_at_the_encoded_count() {
    let source = format!("t = {{{}}}", value_list(usize::from(MAX_TABLE_BATCH_SIZE)));
    let bytecode = parse_unfinalized(&source).expect("one full array batch must compile");
    let set_lists = opcodes(&bytecode, Instr::OP_SET_LIST);
    assert_eq!(set_lists.len(), 1);
    assert_eq!(
        set_lists[0].set_list_count(),
        Some(u16::from(MAX_TABLE_BATCH_SIZE))
    );
    assert_eq!(set_lists[0].c(), 0);

    let source = format!(
        "t = {{{}}}",
        value_list(usize::from(MAX_TABLE_BATCH_SIZE) + 1)
    );
    let bytecode = parse_unfinalized(&source).expect("one more entry must begin the next batch");
    let set_lists = opcodes(&bytecode, Instr::OP_SET_LIST);
    assert_eq!(set_lists.len(), 2);
    assert_eq!(
        (set_lists[0].set_list_count(), set_lists[0].c()),
        (Some(u16::from(MAX_TABLE_BATCH_SIZE)), 0)
    );
    assert_eq!(
        (set_lists[1].set_list_count(), set_lists[1].c()),
        (Some(1), 1)
    );
}

#[test]
fn too_many_upvalues_errors() {
    let err =
        parse_unfinalized(&upvalue_program(200, 56)).expect_err("256 upvalues must be rejected");
    assert!(matches!(
        err.kind(),
        ErrorKind::SyntaxError(SyntaxError::TooManyUpvalues)
    ));
    assert!(err.to_string().contains("limit 255"));
    parse_unfinalized(&upvalue_program(200, 55)).expect("255 upvalues must remain supported");
}

#[test]
fn syntax_depth_limit() {
    std::thread::Builder::new()
        .stack_size(DEEP_PARSE_TEST_STACK_BYTES)
        .spawn(syntax_depth_limit_body)
        .expect("spawning the test thread should succeed")
        .join()
        .expect("syntax depth test thread should not panic");
}

fn syntax_depth_limit_body() {
    let depth = 10_000;
    assert_too_many_syntax_levels(&format!("{}1{}", "(".repeat(depth), ")".repeat(depth)));
    assert_too_many_syntax_levels(&format!("{}{}", "do ".repeat(depth), "end ".repeat(depth)));
    assert_too_many_syntax_levels(&format!("local x = {}1", "- ".repeat(depth)));
    assert_too_many_syntax_levels(&format!(
        "local x = {}1{}",
        "{".repeat(depth),
        "}".repeat(depth)
    ));
    // Nesting inside a suffix still recurses and still hits the cap.
    assert_too_many_syntax_levels(&format!(
        "local x = {}1{}",
        "a[(".repeat(depth),
        ")]".repeat(depth)
    ));
    assert_too_many_syntax_levels(&format!(
        "local x = {}1{}",
        "f(".repeat(depth),
        ")".repeat(depth)
    ));
}

#[test]
fn flat_chains_do_not_consume_syntax_depth() {
    // Reference Lua parses elseif arms and expression suffixes in loops, so
    // chains far longer than MAX_SYNTAX_DEPTH must compile.
    let arms = 1_000;
    parse_unfinalized(&format!(
        "local x = 0 if x == 0 then x = 1 {}else x = 2 end",
        "elseif x == 1 then x = 3 ".repeat(arms)
    ))
    .expect("a long elseif chain must compile");
    parse_unfinalized(&format!("local t = a{}", ".b".repeat(arms)))
        .expect("a long field path must compile");
    parse_unfinalized(&format!("local t = a{}", "[1]".repeat(arms)))
        .expect("a long index path must compile");
    parse_unfinalized(&format!("o{}", ":m()".repeat(arms)))
        .expect("a long method chain must compile");
    parse_unfinalized(&format!("f{}", "()".repeat(arms))).expect("a long call chain must compile");
}

#[test]
fn long_elseif_chain_runs_the_matching_arm() {
    let arms = 300;
    let mut source = String::from(
        "local function assert(c) if not c then error('assertion failed') end end \
         local x = 250 local hit = -1 if x == 0 then hit = 0 ",
    );
    for arm in 1..arms {
        source.push_str(&format!("elseif x == {arm} then hit = {arm} "));
    }
    source.push_str("else hit = -2 end assert(hit == 250)");
    let mut state = State::new();
    crate::Program::compile(&source, None)
        .and_then(|program| state.load(&program))
        .expect("long elseif chain compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("the matching arm runs");
}

#[test]
fn mixed_suffix_chain_preserves_dynamic_call_arguments() {
    let mut state = State::new();
    crate::Program::compile(
        "local function assert(c) if not c then error('assertion failed') end end \
             local leaf = {} \
             function leaf:m(x, y) \
               assert(self == leaf and x == 4 and y == 5) \
               return function(a, b) return a + x, b + y end \
             end \
             local root = {b = {leaf}} \
             local function args() return 4, 5 end \
             local x, y = root.b[1]:m(args())(args()) \
             assert(x == 8 and y == 10)",
        None,
    )
    .and_then(|program| state.load(&program))
    .expect("mixed suffix chain compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("both dynamic calls receive their own arguments");
}

#[test]
fn break_in_nested_function_is_outside_any_loop() {
    // Loop context is per-function: a `break` in a function literal must not
    // bind to the enclosing function's loop.
    let loops = [
        ("while true do ", " end"),
        ("repeat ", " until true"),
        ("for i = 1, 3 do ", " end"),
        ("for k, v in pairs({}) do ", " end"),
    ];
    let bodies = [
        "local f = function() break end",
        "local f = function() local a = 1 local b = 2 local c = 3 local d = 4 \
         local e = 5 local g = 6 local h = 7 break end",
        "local function f() if true then break end end",
    ];
    for (open, close) in loops {
        for body in bodies {
            let source = format!("{open}{body}{close}");
            let err = parse_unfinalized(&source)
                .expect_err("break inside a nested function's non-loop body must fail");
            assert!(
                matches!(
                    err.kind(),
                    ErrorKind::SyntaxError(SyntaxError::BreakOutsideLoop)
                ),
                "{source}: unexpected error {err:?}"
            );
        }
    }
}

#[test]
fn loops_inside_and_around_nested_functions_keep_their_own_breaks() {
    let source = "local function assert(c) if not c then error('assertion failed') end end \
        local n = 0 \
        while true do \
          n = n + 1 \
          local f = function() local s = 0 for i = 1, 10 do s = s + i if i == 3 then break end end return s end \
          assert(f() == 6) \
          if n > 2 then break end \
        end \
        assert(n == 3)";
    let mut state = State::new();
    crate::Program::compile(source, None)
        .and_then(|program| state.load(&program))
        .expect("source compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("both loops break correctly");
}

#[test]
fn syntax_depth_headroom() {
    // This test is the empirical check that MAX_SYNTAX_DEPTH fits the stack a
    // debug `cargo test` thread gets (2MB by default, not the 8MB main stack).
    // It therefore has to exercise the *fattest* cycle, not the cheapest.
    let depth = 190;
    parse_unfinalized(&format!("{}{}", "do ".repeat(depth), "end ".repeat(depth)))
        .expect("statement nesting below the limit must compile");

    // Nested parens are the worst case: each level descends through both the
    // parse_expr and parse_unary wrappers plus the whole precedence ladder,
    // and charges 2 depth ticks.
    let paren_depth = 95;
    parse_unfinalized(&format!(
        "local x = {}1{}",
        "(".repeat(paren_depth),
        ")".repeat(paren_depth)
    ))
    .expect("paren nesting below the limit must compile");

    // Nested call arguments and indexes retain additional suffix-parser
    // frames. Their active contents charge SYNTAX_SUFFIX_COST extra ticks.
    let suffix_depth = depth / (2 + SYNTAX_SUFFIX_COST as usize);
    parse_unfinalized(&format!(
        "local x = {}1{}",
        "f(".repeat(suffix_depth),
        ")".repeat(suffix_depth)
    ))
    .expect("call nesting below the limit must compile");
    parse_unfinalized(&format!(
        "local x = {}1{}",
        "a[".repeat(suffix_depth),
        "]".repeat(suffix_depth)
    ))
    .expect("index nesting below the limit must compile");
}

// ---------------------------------------------------------------------------
// Constant folding
// ---------------------------------------------------------------------------

#[test]
fn constant_folding_collapses_literal_arithmetic() {
    for (source, op) in [
        ("return 60 * 1000", ArithOp::Mul),
        ("return 1 + 2 + 3", ArithOp::Add),
        ("return 10 - 4", ArithOp::Sub),
        ("return 10 / 4", ArithOp::Div),
        ("return 10 % 3", ArithOp::Mod),
    ] {
        let chunk = parse_unfinalized(source).expect("fold fixture compiles");
        assert!(!has_arith(&chunk, op), "{source}: {:?}", chunk.code);
    }
    // `^` is never folded: runtime `^` is charged, a folded literal would not be.
    assert!(has_arith(
        &parse_unfinalized("return 2 ^ 10").unwrap(),
        ArithOp::Pow
    ));
    assert!(has_arith(
        &parse_unfinalized("return 2 ^ 0.5 + 1").unwrap(),
        ArithOp::Add
    ));
    for source in ["return -5", "return -(2 * 3)"] {
        assert!(opcodes(&parse_unfinalized(source).unwrap(), Instr::OP_UNM).is_empty());
    }

    assert_eq!(eval_number("return 60 * 1000 + 8"), 60008.0);
    assert_eq!(eval_number("return -5 % 3"), 1.0);
    assert_eq!(eval_number("return 5 % -3"), -1.0);
    assert_eq!(eval_number("return 2 ^ 10"), 1024.0);
    assert_eq!(eval_number("return - -5"), 5.0);
}

#[test]
fn constant_folding_inserts_literals_at_its_reduction_events() {
    // Operands pool as the lexer meets them, each fold result when its
    // operator reduces: pool contents are snapshot data.
    for (source, expected) in [
        ("x = 5 + 6", vec![5.0, 6.0, 11.0]),
        ("return 1 + 2 * 3", vec![1.0, 2.0, 3.0, 6.0, 7.0]),
        ("x = 1 .. 2 + 3", vec![1.0, 2.0, 3.0, 5.0]),
        (
            "return 60 * 1000 + 8",
            vec![60.0, 1000.0, 60000.0, 8.0, 60008.0],
        ),
        ("x = -5^2", vec![5.0, 2.0]),
    ] {
        let chunk = parse_unfinalized(source).expect("fold fixture compiles");
        assert_eq!(chunk.number_literals, expected, "{source}");
    }
}

#[test]
fn constant_folding_refuses_nan_and_negative_zero_results() {
    // NaN payloads and -0.0 collapse in the ==-deduped literal pool, so
    // these stay runtime operations (reference lcode.c's guards).
    assert!(has_arith(
        &parse_unfinalized("return 0 / 0").unwrap(),
        ArithOp::Div
    ));
    assert!(!opcodes(&parse_unfinalized("return -0").unwrap(), Instr::OP_UNM).is_empty());
    assert!(has_arith(
        &parse_unfinalized("return 0 * -1").unwrap(),
        ArithOp::Mul
    ));
    // Infinity is a normal foldable value.
    assert!(!has_arith(
        &parse_unfinalized("return 1 / 0").unwrap(),
        ArithOp::Div
    ));
    assert_eq!(eval_number("return 1 / 0"), f64::INFINITY);
    // A refused fold stays charged for its parents too.
    assert!(!opcodes(&parse_unfinalized("return -(-0)").unwrap(), Instr::OP_UNM).is_empty());
}

#[test]
fn constant_folding_only_fires_on_literal_operands() {
    assert!(has_arith(
        &parse_unfinalized("local x = 1 return x + 2").unwrap(),
        ArithOp::Add
    ));
    assert!(has_arith(
        &parse_unfinalized("local x = 1 return 2 + x").unwrap(),
        ArithOp::Add
    ));
    // Mixed: the literal half folds, the variable half stays.
    let chunk = parse_unfinalized("local x = 1 return x + 2 * 3").expect("mixed compiles");
    assert!(has_arith(&chunk, ArithOp::Add));
    assert!(!has_arith(&chunk, ArithOp::Mul));
    // Parentheses keep foldability; logical joins do not.
    assert!(!has_arith(
        &parse_unfinalized("return 1 + (2 * 3)").unwrap(),
        ArithOp::Add
    ));
    let chunk = parse_unfinalized("local a = 5 return (a or 2) * 3").unwrap();
    assert!(has_arith(&chunk, ArithOp::Mul));
}

#[test]
fn folding_keeps_line_info_synced_with_code() {
    for source in [
        "return 60 * 1000 + 8",
        "x = -5^2",
        "local n = 0\nwhile n < 3 do\n  n = n + 1 + 2\nend\nreturn n",
        "local a = 5 if a or 1 < 2 then return 1 end return 0",
        "local f = function() return 2 * 3 end\nif f() >= 6 then return 1 end",
    ] {
        let chunk = parse_unfinalized(source).expect("line-info fixture compiles");
        assert_line_info_matches_code_len(&chunk);
    }
}

#[test]
fn compare_branches_keep_comparison_semantics() {
    assert_eq!(
        eval_number("local n = 0 while n < 3 do n = n + 1 end return n"),
        3.0
    );
    assert_eq!(
        eval_number("local n = 0 repeat n = n + 1 until n >= 4 return n"),
        4.0
    );
    assert_eq!(
        eval_number("if 'a' < 'b' then return 1 else return 2 end"),
        1.0
    );
    // NaN compares false through the branch, like the value form.
    assert_eq!(
        eval_number("local nan = 0/0 if nan == nan then return 1 else return 2 end"),
        2.0
    );
    assert_eq!(
        eval_number("local nan = 0/0 if nan <= nan then return 1 else return 2 end"),
        2.0
    );
    // The branch still raises the comparison type error.
    let mut state = State::new();
    crate::Program::compile("if 1 < 'x' then return 1 end return 0", None)
        .and_then(|program| state.load(&program))
        .expect("type-error fixture compiles");
    assert!(
        state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).is_err(),
        "number/string comparison must still error in a branch"
    );
}

#[test]
fn if_arms_ending_in_return_emit_no_end_jump() {
    // Every arm returns, so no arm can fall through to the end of the `if`.
    for source in [
        "local x = 1 if x then return 1 else return 2 end",
        "local x = 1 if x == 1 then return 1 elseif x == 2 then return 2 else return 3 end",
        "local x = 1 if x then local y = x return y else return 2 end",
    ] {
        let chunk = parse_unfinalized(source).expect("compiles");
        assert!(opcodes(&chunk, Instr::OP_JMP).is_empty(), "{source}");
    }
    assert_eq!(
        eval_number(
            "local x = 2 if x == 1 then return 1 elseif x == 2 then return 2 else return 3 end"
        ),
        2.0
    );
    assert_eq!(
        eval_number("local x = 5 if x == 1 then return 1 elseif x == 2 then return 2 end return 9"),
        9.0
    );
    // A local captured in a returning arm still compiles and runs.
    assert_eq!(
        eval_number(
            "local x = 1 if x then local y = 4 local f = function() return y end return f() else return 2 end"
        ),
        4.0
    );
}

#[test]
fn if_arm_ending_in_nested_if_keeps_its_end_jump() {
    let source = "local a, b, r = true, false, 0 \
                  if a then if b then return 1 elseif b == nil then return 2 end \
                  else r = 7 end return r + 3";
    assert!(!opcodes(&parse_unfinalized(source).unwrap(), Instr::OP_JMP).is_empty());
    assert_eq!(eval_number(source), 3.0);
    let source = "local a, b = true, false \
                  if a then if b then return 1 end else return 7 end return 3";
    assert_eq!(eval_number(source), 3.0);
}

#[test]
fn if_arm_does_not_inherit_a_do_blocks_return_status() {
    let source = "local a = true if a then do return 1 end else return 7 end return 3";
    assert!(!opcodes(&parse_unfinalized(source).unwrap(), Instr::OP_JMP).is_empty());
    assert_eq!(eval_number(source), 1.0);

    for tail in ["", "r = 2"] {
        let source = format!(
            "local a, b, r = true, false, 0 \
             if a then do if b then return 1 end end {tail} \
             else r = 7 end return r + 3"
        );
        assert!(!opcodes(&parse_unfinalized(&source).unwrap(), Instr::OP_JMP).is_empty());
        assert_eq!(
            eval_number(&source),
            if tail.is_empty() { 3.0 } else { 5.0 }
        );
    }
}

#[test]
fn unfinished_long_comment_is_a_syntax_error() {
    for source in [
        "x = 1 --[[ never closed\nx = 2",
        "--[==[ wrong level ]=]\nreturn 1",
    ] {
        let err =
            parse_unfinalized(source).expect_err("an unfinished long comment must not compile");
        assert!(
            matches!(
                err.kind(),
                ErrorKind::SyntaxError(SyntaxError::UnfinishedLongComment)
            ),
            "{source:?}: {err:?}"
        );
        assert!(
            err.to_string().contains("unfinished long comment"),
            "{source:?}: {err}"
        );
    }
}

#[test]
fn lparen_on_a_later_line_continues_the_call_regardless_of_comments() {
    let expected = parse_unfinalized("f() (g)()").expect("same-line form compiles");
    for source in [
        "f()\n(g)()",
        "f() -- note\n(g)()",
        "f()\n-- note\n(g)()",
        "f() --[[ a\nb ]] (g)()",
        "f() --[[ same line ]] (g)()",
    ] {
        let chunk = parse_unfinalized(source).unwrap_or_else(|e| panic!("{source:?}: {e:?}"));
        assert_eq!(chunk.code, expected.code, "{source:?}");
    }
}

#[test]
fn while_condition_constants_still_loop() {
    assert_eq!(
        eval_number("local n = 0\nwhile 2 * 3 do\n  n = n + 1\n  if n >= 2 then return n end\nend"),
        2.0
    );
}

fn nested_function_chain(depth: usize) -> String {
    let nested_functions = depth - 1;
    // The innermost body stays empty: an expression there would add a syntax
    // level of its own and hit the syntax depth limit one chunk early.
    format!(
        "{}{}",
        "function f() ".repeat(nested_functions),
        "end ".repeat(nested_functions)
    )
}

#[test]
fn chunk_nesting_limit_is_enforced_by_the_parser() {
    std::thread::Builder::new()
        .stack_size(DEEP_PARSE_TEST_STACK_BYTES)
        .spawn(|| {
            let source = nested_function_chain(MAX_CHUNK_NESTING);
            let bytecode =
                parse_unfinalized(&source).expect("the deepest allowed chunk chain must compile");
            let error = parse_unfinalized(&nested_function_chain(MAX_CHUNK_NESTING + 1))
                .expect_err("one more nested chunk must fail in the parser");
            assert!(matches!(
                error.kind(),
                ErrorKind::SyntaxError(SyntaxError::TooManySyntaxLevels)
            ));
            assert_eq!(chunk_depth(&bytecode), MAX_CHUNK_NESTING);
        })
        .expect("spawning the chunk nesting test thread should succeed")
        .join()
        .unwrap_or_else(|payload| {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic payload");
            panic!("chunk nesting test thread panicked: {message}");
        });
}

fn chunk_depth(chunk: &Bytecode) -> usize {
    1 + chunk.nested.first().map_or(0, |nested| chunk_depth(nested))
}

#[test]
fn return_255_literals_uses_a_fixed_count() {
    let source = format!("return {}", vec!["1"; 255].join(","));
    let bytecode = parse_unfinalized(&source).expect("255 literal returns remain accepted");
    assert_eq!(
        bytecode.code.last().copied(),
        Some(Instr::ret(0, RetCount::Fixed(0)))
    );
    let returns = opcodes(&bytecode, Instr::OP_RETURN);
    assert_eq!(returns[0], Instr::ret(0, RetCount::Fixed(255)));
    assert_eq!(opcodes(&bytecode, Instr::OP_LOAD_NUM).len(), 255);
}

#[test]
fn transitive_capture_indices_follow_first_reference_order() {
    let chunk =
        parse_unfinalized("local a, b; return function() return function() return b, a, b end end")
            .expect("transitive capture fixture must compile");
    let middle = &chunk.nested[0];
    let inner = &middle.nested[0];
    assert_eq!(
        middle.upvalues,
        vec![UpvalueDesc::Local(1), UpvalueDesc::Local(0)]
    );
    assert_eq!(
        inner.upvalues,
        vec![UpvalueDesc::Upvalue(0), UpvalueDesc::Upvalue(1)]
    );
    assert!(chunk.upvalues.is_empty());
}

#[test]
fn block_exit_closes_a_shadowed_capture_slot() {
    let chunk = parse_unfinalized(
        "local x; do local x; first = function() return x end end; \
         second = function() return x end",
    )
    .expect("shadowed capture fixture must compile");
    assert_eq!(chunk.nested[0].upvalues, vec![UpvalueDesc::Local(1)]);
    assert_eq!(chunk.nested[1].upvalues, vec![UpvalueDesc::Local(0)]);
    let closures: Vec<usize> = chunk
        .code
        .iter()
        .enumerate()
        .filter(|(_, inst)| inst.opcode() == Instr::OP_CLOSURE)
        .map(|(pc, _)| pc)
        .collect();
    let closes: Vec<(usize, Instr)> = chunk
        .code
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, inst)| inst.opcode() == Instr::OP_CLOSE)
        .collect();
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0].1, Instr::close(1));
    assert!(closures[0] < closes[0].0 && closes[0].0 < closures[1]);
}
