use std::sync::Arc;

use super::Bytecode;
use super::DEEP_PARSE_TEST_STACK_BYTES;
use super::Instr;
use super::Parser;
use super::TokenStream;
use super::parse_unfinalized;
use crate::State;
use crate::compiler::UpvalueDesc;
use crate::error::{ErrorKind, SyntaxError};
use crate::instr::{ArgCount, Builtin, RetCount};
use crate::limits::{
    MAX_ASSIGNMENT_OFFSET, MAX_CHUNK_NESTING, MAX_EXPRESSIONS, MAX_FIXED_ARGS, MAX_FIXED_RESULTS,
    MAX_LITERALS, MAX_LOCALS, MAX_NESTED, MAX_SOURCE_BYTES, MAX_TABLE_BATCH_SIZE,
    MAX_TABLE_TEMPLATE_FIELDS, MAX_TABLE_TEMPLATES, SYNTAX_SUFFIX_COST,
};

/// Recursively clear line_info from a chunk and its nested chunks.
fn clear_line_info(chunk: &mut Bytecode) {
    chunk.line_info.clear();
    for nested in &mut chunk.nested {
        let inner = Arc::get_mut(nested).expect("test fixture should own its nested chunks");
        clear_line_info(inner);
    }
}

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

fn check_it(input: &str, mut output: Bytecode) {
    // Top-level chunks are always vararg functions
    output.is_vararg = true;
    let mut actual = parse_unfinalized(input).unwrap();
    // Clear line_info for comparison (tests were written before line tracking existed)
    clear_line_info(&mut actual);
    assert_eq!(actual, output);
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

#[test]
fn checked_jump_offset_accepts_i16_boundaries_only() {
    let parser = Parser {
        input: TokenStream::new(b""),
        funcs: super::FunctionStack {
            frames: vec![super::state::FuncState::new(None, 0, false)],
        },
        source_name: None,
        syntax_depth: 0,
    };

    assert_eq!(parser.checked_jump_offset(0, 32_768).unwrap(), i16::MAX);
    assert!(parser.checked_jump_offset(0, 32_769).is_err());
    assert_eq!(parser.checked_jump_offset(32_767, 0).unwrap(), i16::MIN);
    assert!(parser.checked_jump_offset(32_768, 0).is_err());
}

#[test]
fn test01() {
    // 5 + 6 folds at parse time; the operand literals stay pooled.
    let text = "x = 5 + 6";
    let out = Bytecode {
        code: vec![
            Instr::push_num(2),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![5.0, 6.0, 11.0],
        string_literals: vec!["x".into()],
        ..Bytecode::default()
    };
    check_it(text, out);
}

#[test]
fn test02() {
    // ^ binds tighter than unary minus. `^` is never folded (a folded literal
    // would not be charged), so the negation has no literal to fold into.
    let text = "x = -5^2";
    let out = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::push_num(1),
            Instr::pow(),
            Instr::negate(),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![5.0, 2.0],
        string_literals: vec!["x".into()],
        ..Bytecode::default()
    };
    check_it(text, out);
}

#[test]
fn test03() {
    let text = "x = 5 + true .. 'hi'";
    let out = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::push_bool(true),
            Instr::add(),
            Instr::push_string(1),
            Instr::concat(2),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![5.0],
        string_literals: vec!["x".into(), "hi".into()],
        ..Bytecode::default()
    };
    check_it(text, out);
}

#[test]
fn test04() {
    // 2 + 3 folds to 5 before the concat.
    let text = "x = 1 .. 2 + 3";
    let output = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::push_num(3),
            Instr::concat(2),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![1.0, 2.0, 3.0, 5.0],
        string_literals: vec!["x".into()],
        ..Bytecode::default()
    };
    check_it(text, output);
}

#[test]
fn concat_chain_emits_single_n_ary_concat() {
    let text = r#"x = "a" .. "b" .. "c" .. "d""#;
    let output = Bytecode {
        code: vec![
            Instr::push_string(1),
            Instr::push_string(2),
            Instr::push_string(3),
            Instr::push_string(4),
            Instr::concat(4),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ],
        string_literals: vec!["x".into(), "a".into(), "b".into(), "c".into(), "d".into()],
        ..Bytecode::default()
    };
    check_it(text, output);
}

#[test]
fn test05() {
    // -3 folds to a literal; the `^` itself is left to runtime.
    let text = "x = 2^-3";
    let output = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::push_num(2),
            Instr::pow(),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![2.0, 3.0, -3.0],
        string_literals: vec!["x".into()],
        ..Bytecode::default()
    };
    check_it(text, output);
}

#[test]
fn test06() {
    let text = "x=  not not 1";
    let output = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::not(),
            Instr::not(),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![1.0],
        string_literals: vec!["x".into()],
        ..Bytecode::default()
    };
    check_it(text, output);
}

#[test]
fn test07() {
    let text = "a = 5";
    let output = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![5.0],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    check_it(text, output);
}

#[test]
fn test08() {
    let text = "x = true and false";
    let output = Bytecode {
        code: vec![
            Instr::push_bool(true),
            Instr::branch_false_keep(2),
            Instr::pop(),
            Instr::push_bool(false),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ],
        string_literals: vec!["x".into()],
        ..Bytecode::default()
    };
    check_it(text, output);
}

#[test]
fn test09() {
    let text = "x =  5 or nil and true";
    let code = vec![
        Instr::push_num(0),
        Instr::branch_true_keep(5),
        Instr::pop(),
        Instr::push_nil(),
        Instr::branch_false_keep(2),
        Instr::pop(),
        Instr::push_bool(true),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let output = Bytecode {
        code,
        number_literals: vec![5.0],
        string_literals: vec!["x".into()],
        ..Bytecode::default()
    };
    check_it(text, output);
}

#[test]
fn test10() {
    let text = "if true then a = 5 end";
    let code = vec![
        Instr::push_bool(true),
        Instr::branch_false(2),
        Instr::push_num(0),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![5.0],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test11() {
    let text = "if true then a = 5 if true then b = 4 end end";
    let code = vec![
        Instr::push_bool(true),
        Instr::branch_false(6),
        Instr::push_num(0),
        Instr::set_global(0, None),
        Instr::push_bool(true),
        Instr::branch_false(2),
        Instr::push_num(1),
        Instr::set_global(1, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![5.0, 4.0],
        string_literals: vec!["a".into(), "b".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test12() {
    let text = "if true then a = 5 else a = 4 end";
    let code = vec![
        Instr::push_bool(true),
        Instr::branch_false(3),
        Instr::push_num(0),
        Instr::set_global(0, None),
        Instr::jump(2),
        Instr::push_num(1),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![5.0, 4.0],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test13() {
    // The elseif condition ends in a comparison, so its branch fuses.
    let text = "if true then a = 5 elseif 6 == 7 then a = 3 else a = 4 end";
    let code = vec![
        Instr::push_bool(true),
        Instr::branch_false(3),
        Instr::push_num(0),
        Instr::set_global(0, None),
        Instr::jump(8),
        Instr::push_num(1),
        Instr::push_num(2),
        Instr::branch_false_equal(3),
        Instr::push_num(3),
        Instr::set_global(0, None),
        Instr::jump(2),
        Instr::push_num(4),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![5.0, 6.0, 7.0, 3.0, 4.0],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test14() {
    // The loop condition ends in a comparison, so the test branch fuses.
    let text = "while a < 10 do a = a + 1 end";
    let code = vec![
        Instr::get_global(0, None),
        Instr::push_num(0),
        Instr::branch_false_less(5),
        Instr::get_global(0, None),
        Instr::push_num(1),
        Instr::add(),
        Instr::set_global(0, None),
        Instr::jump(-8),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![10.0, 1.0],
        string_literals: vec!["a".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test15() {
    // repeat local x = 5 until a == b - uncaptured body locals need no
    // CloseUpvalues, so the comparison fuses with the backward branch
    let text = "repeat local x = 5 until a == b y = 4";
    let code = vec![
        Instr::push_num(0),
        Instr::set_local(0),
        Instr::get_global(0, None),
        Instr::get_global(1, None),
        Instr::branch_false_equal(-5),
        Instr::push_num(1),
        Instr::set_global(2, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![5.0, 4.0],
        string_literals: vec!["a".into(), "b".into(), "y".into()],
        num_locals: 1,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test16() {
    let text = "local i i = 2";
    let code = vec![
        Instr::push_nil(),
        Instr::set_local(0),
        Instr::push_num(0),
        Instr::set_local(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![2.0],
        num_locals: 1,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test17() {
    let text = "local i, j print(j)";
    let code = vec![
        Instr::push_nil(),
        Instr::push_nil(),
        Instr::set_local(1),
        Instr::set_local(0),
        Instr::get_builtin(Builtin::Print),
        // Fixed calls leave their retired call-base mark as a nop; finalize
        // strips it (raw parser output is what these tests pin).
        Instr::nop(),
        Instr::get_local(1),
        Instr::call(ArgCount::Fixed(1), RetCount::Fixed(0)),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        num_locals: 2,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test18() {
    let text = "local i do local i x = i end x = i";
    let code = vec![
        Instr::push_nil(),
        Instr::set_local(0),
        Instr::push_nil(),
        Instr::set_local(1),
        Instr::get_local(1),
        Instr::set_global(0, None),
        Instr::get_local(0),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        string_literals: vec!["x".into()],
        num_locals: 2,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test19() {
    let text = "do local i x = i end x = i";
    let code = vec![
        Instr::push_nil(),
        Instr::set_local(0),
        Instr::get_local(0),
        Instr::set_global(0, None),
        Instr::get_global(1, None),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        string_literals: vec!["x".into(), "i".into()],
        num_locals: 1,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test20() {
    let text = "local i if false then local i else x = i end";
    let code = vec![
        Instr::push_nil(),
        Instr::set_local(0),
        Instr::push_bool(false),
        Instr::branch_false(3),
        Instr::push_nil(),
        Instr::set_local(1),
        Instr::jump(2),
        Instr::get_local(0),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        string_literals: vec!["x".into()],
        num_locals: 2,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test21() {
    let text = "for i = 1,5 do x = i end";
    let code = vec![
        Instr::push_num(0),
        Instr::push_num(1),
        Instr::push_num(0),
        Instr::for_prep(0, 3),
        Instr::get_local(3),
        Instr::set_global(0, None),
        Instr::for_loop(0, -3),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0, 5.0],
        string_literals: vec!["x".into()],
        num_locals: 4,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn generic_for_bytecode_keeps_control_and_visible_slots() {
    let text = "for k, v in pairs(t) do x = k end";
    let code = vec![
        Instr::get_builtin(Builtin::Pairs),
        Instr::nop(),
        Instr::get_global(0, None),
        Instr::call(ArgCount::Fixed(1), RetCount::Fixed(3)),
        Instr::tfor_prep(0),
        Instr::tfor_call(0, 2, None),
        Instr::tfor_loop(0, 3),
        Instr::get_local(3),
        Instr::set_global(1, None),
        Instr::jump(-5),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        string_literals: vec!["t".into(), "x".into()],
        num_locals: 5,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn for_control_function_captures_enclosing_same_named_local() {
    let chunk = parse_unfinalized("local i = 10; for i = (function() return i end)(), 10 do end")
        .expect("for control fixture must compile");
    let function = &chunk.nested[0];

    assert_eq!(function.upvalues, vec![UpvalueDesc::Local(0)]);
}

#[test]
fn test22() {
    let text = "a, b = 1";
    let code = vec![
        Instr::push_num(0),
        Instr::push_nil(),
        Instr::set_global(1, None),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0],
        string_literals: vec!["a".into(), "b".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test23() {
    let text = "a, b = 1, 2";
    let code = vec![
        Instr::push_num(0),
        Instr::push_num(1),
        Instr::set_global(1, None),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0, 2.0],
        string_literals: vec!["a".into(), "b".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test24() {
    let text = "a, b = 1, 2, 3";
    let code = vec![
        Instr::push_num(0),
        Instr::push_num(1),
        Instr::push_num(2),
        Instr::pop(),
        Instr::set_global(1, None),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0, 2.0, 3.0],
        string_literals: vec!["a".into(), "b".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test25() {
    let text = "puts()";
    let code = vec![
        Instr::get_global(0, None),
        Instr::nop(),
        Instr::call(ArgCount::Fixed(0), RetCount::Fixed(0)),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        string_literals: vec!["puts".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test26() {
    let text = "y = {x = 5,}";
    let code = vec![
        Instr::new_table(),
        Instr::push_num(0),
        Instr::init_field(0, 1),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![5.0],
        string_literals: vec!["y".into(), "x".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn table_constructor_presizes_larger_literals() {
    let text = "y = {a = 1, b = 2, c = 3, d = 4, e = 5}";
    let code = vec![
        Instr::new_table_template(0),
        Instr::push_num(0),
        Instr::init_field_pinned(1, 0),
        Instr::push_num(1),
        Instr::init_field_pinned(2, 1),
        Instr::push_num(2),
        Instr::init_field_pinned(3, 2),
        Instr::push_num(3),
        Instr::init_field_pinned(4, 3),
        Instr::push_num(4),
        Instr::init_field_pinned(5, 4),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0, 2.0, 3.0, 4.0, 5.0],
        string_literals: vec![
            "y".into(),
            "a".into(),
            "b".into(),
            "c".into(),
            "d".into(),
            "e".into(),
        ],
        table_templates: vec![vec![1, 2, 3, 4, 5]],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn duplicate_table_constructor_keys_use_presized_table() {
    let text = "y = {a = 1, b = 2, c = 3, d = 4, a = 5}";
    let code = vec![
        Instr::new_table_presized(5),
        Instr::push_num(0),
        Instr::init_field(0, 1),
        Instr::push_num(1),
        Instr::init_field(0, 2),
        Instr::push_num(2),
        Instr::init_field(0, 3),
        Instr::push_num(3),
        Instr::init_field(0, 4),
        Instr::push_num(4),
        Instr::init_field(0, 1),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0, 2.0, 3.0, 4.0, 5.0],
        string_literals: vec!["y".into(), "a".into(), "b".into(), "c".into(), "d".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn computed_table_constructor_keys_use_presized_table() {
    let text = "y = {a = 1, b = 2, c = 3, d = 4, [k] = 5}";
    let code = vec![
        Instr::new_table_presized(5),
        Instr::push_num(0),
        Instr::init_field(0, 1),
        Instr::push_num(1),
        Instr::init_field(0, 2),
        Instr::push_num(2),
        Instr::init_field(0, 3),
        Instr::push_num(3),
        Instr::init_field(0, 4),
        Instr::get_global(5, None),
        Instr::push_num(4),
        Instr::init_index(0),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        number_literals: vec![1.0, 2.0, 3.0, 4.0, 5.0],
        string_literals: vec![
            "y".into(),
            "a".into(),
            "b".into(),
            "c".into(),
            "d".into(),
            "k".into(),
        ],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn table_constructor_expands_only_final_bare_call_or_vararg() {
    let final_call = parse_unfinalized("y = {7, f()}").unwrap();
    assert!(
        final_call
            .code
            .iter()
            .any(|instr| instr == &Instr::new_table_tracked(0))
    );
    assert!(
        final_call
            .code
            .iter()
            .any(|instr| instr == &Instr::call(ArgCount::Fixed(0), RetCount::All))
    );
    assert!(
        final_call
            .code
            .iter()
            .any(|instr| instr == &Instr::set_list(0))
    );

    let non_final_call = parse_unfinalized("y = {f(), 7}").unwrap();
    assert!(
        non_final_call
            .code
            .iter()
            .any(|instr| instr == &Instr::new_table())
    );
    assert!(
        non_final_call
            .code
            .iter()
            .any(|instr| instr == &Instr::call(ArgCount::Fixed(0), RetCount::Fixed(1)))
    );
    assert!(
        non_final_call
            .code
            .iter()
            .any(|instr| instr == &Instr::set_list(2))
    );

    let final_vararg = parse_unfinalized("y = {99, ...}").unwrap();
    assert!(
        final_vararg
            .code
            .iter()
            .any(|instr| instr == &Instr::new_table_tracked(0))
    );
    assert!(
        final_vararg
            .code
            .iter()
            .any(|instr| instr == &Instr::vararg(RetCount::All))
    );
    assert!(
        final_vararg
            .code
            .iter()
            .any(|instr| instr == &Instr::set_list(0))
    );

    let non_final_vararg = parse_unfinalized("y = {..., 99}").unwrap();
    assert!(
        non_final_vararg
            .code
            .iter()
            .any(|instr| instr == &Instr::vararg(RetCount::Fixed(1)))
    );
    assert!(
        non_final_vararg
            .code
            .iter()
            .any(|instr| instr == &Instr::set_list(2))
    );
}

#[test]
fn table_constructor_keyed_final_field_prevents_expansion() {
    let keyed_last = parse_unfinalized("y = {f(), k = 7}").unwrap();
    assert!(
        keyed_last
            .code
            .iter()
            .any(|instr| instr == &Instr::new_table())
    );
    assert!(
        keyed_last
            .code
            .iter()
            .any(|instr| instr == &Instr::call(ArgCount::Fixed(0), RetCount::Fixed(1)))
    );
    assert!(
        keyed_last
            .code
            .iter()
            .any(|instr| instr == &Instr::set_list(1))
    );

    let call_last = parse_unfinalized("y = {k = 7, f()}").unwrap();
    assert!(
        call_last
            .code
            .iter()
            .any(|instr| instr == &Instr::new_table_tracked(1))
    );
    assert!(
        call_last
            .code
            .iter()
            .any(|instr| instr == &Instr::call(ArgCount::Fixed(0), RetCount::All))
    );

    let dynamic_call = parse_unfinalized("y = {f(g())}").unwrap();
    assert!(
        dynamic_call
            .code
            .iter()
            .any(|instr| { instr == &Instr::call(ArgCount::Dynamic, RetCount::All) })
    );

    // The tracked hint counts named and computed keys, never positional ones.
    let capacity = parse_unfinalized("y = {1, 2, a = 1, [k] = 2, f()}").unwrap();
    assert!(
        capacity
            .code
            .iter()
            .any(|instr| instr == &Instr::new_table_tracked(2))
    );

    // Positional fields alone need no hash hint; SET_LIST reserves the array.
    let positional = parse_unfinalized("y = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10}").unwrap();
    assert!(
        positional
            .code
            .iter()
            .any(|instr| instr == &Instr::new_table())
    );
    assert!(
        !positional
            .code
            .iter()
            .any(|instr| instr.opcode() == Instr::OP_NEW_TABLE_PRESIZED)
    );
}

#[test]
fn table_constructor_does_not_expand_non_bare_tails() {
    // A parenthesized call is truncated to one value, so it is not a bare tail
    // and must not be patched to multi-return or use a tracked constructor.
    let paren_call = parse_unfinalized("y = {(f())}").unwrap();
    assert!(
        paren_call
            .code
            .iter()
            .all(|instr| instr.opcode() != Instr::OP_NEW_TABLE_TRACKED)
    );
    assert!(
        paren_call
            .code
            .iter()
            .any(|instr| instr == &Instr::call(ArgCount::Fixed(0), RetCount::Fixed(1)))
    );
    assert!(
        paren_call
            .code
            .iter()
            .any(|instr| instr == &Instr::set_list(1))
    );

    // A field access ends in GetField, not a call/vararg tail.
    let field_access = parse_unfinalized("y = {f().x}").unwrap();
    assert!(
        field_access
            .code
            .iter()
            .all(|instr| instr.opcode() != Instr::OP_NEW_TABLE_TRACKED)
    );
    assert!(
        field_access
            .code
            .iter()
            .any(|instr| instr == &Instr::set_list(1))
    );

    // A parenthesized vararg is truncated to one value (the top-level chunk is
    // vararg), so it stays Vararg(1) and does not expand.
    let paren_vararg = parse_unfinalized("y = {(...)}").unwrap();
    assert!(
        paren_vararg
            .code
            .iter()
            .all(|instr| instr.opcode() != Instr::OP_NEW_TABLE_TRACKED)
    );
    assert!(
        paren_vararg
            .code
            .iter()
            .any(|instr| instr == &Instr::vararg(RetCount::Fixed(1)))
    );
    assert!(
        paren_vararg
            .code
            .iter()
            .any(|instr| instr == &Instr::set_list(1))
    );
}

#[test]
fn test27() {
    let text = "local x = t.x.y";
    let code = vec![
        Instr::get_global(0, None),
        Instr::get_field(1, None),
        Instr::get_field(2, None),
        Instr::set_local(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        string_literals: vec!["t".into(), "x".into(), "y".into()],
        num_locals: 1,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test28() {
    let text = "x = function () end";
    let code = vec![
        Instr::closure(0),
        Instr::set_global(0, None),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let string_literals = vec!["x".into()];
    let nested = vec![Arc::new(Bytecode {
        code: vec![Instr::ret(RetCount::Fixed(0))],
        defined_line: 1,
        ..Bytecode::default()
    })];
    let chunk = Bytecode {
        code,
        string_literals,
        nested,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test29() {
    let text = "x = function () local y = 7 end";
    let inner_chunk = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::set_local(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![7.0],
        num_locals: 1,
        defined_line: 1,
        ..Bytecode::default()
    };
    let outer_chunk = Bytecode {
        code: vec![
            Instr::closure(0),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ],
        string_literals: vec!["x".into()],
        nested: vec![Arc::new(inner_chunk)],
        ..Bytecode::default()
    };
    check_it(text, outer_chunk);
}

#[test]
fn test30() {
    let text = "
    z = function () local z = 21 end
    x = function ()
        local y = function () end
        print(y)
    end";
    let z = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::set_local(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![21.0],
        num_locals: 1,
        defined_line: 2,
        ..Bytecode::default()
    };
    let y = Bytecode {
        code: vec![Instr::ret(RetCount::Fixed(0))],
        defined_line: 4,
        ..Bytecode::default()
    };
    let x = Bytecode {
        code: vec![
            Instr::closure(0),
            Instr::set_local(0),
            Instr::get_builtin(Builtin::Print),
            Instr::nop(),
            Instr::get_local(0),
            Instr::call(ArgCount::Fixed(1), RetCount::Fixed(0)),
            Instr::ret(RetCount::Fixed(0)),
        ],
        nested: vec![Arc::new(y)],
        num_locals: 1,
        defined_line: 3,
        ..Bytecode::default()
    };
    let outer_chunk = Bytecode {
        code: vec![
            Instr::closure(0),
            Instr::set_global(0, None),
            Instr::closure(1),
            Instr::set_global(1, None),
            Instr::ret(RetCount::Fixed(0)),
        ],
        nested: vec![Arc::new(z), Arc::new(x)],
        string_literals: vec!["z".into(), "x".into()],
        ..Bytecode::default()
    };
    check_it(text, outer_chunk);
}

#[test]
fn test31() {
    let text = "local s = type(4)";
    let code = vec![
        Instr::get_builtin(Builtin::Type),
        Instr::nop(),
        Instr::push_num(0),
        Instr::call(ArgCount::Fixed(1), RetCount::Fixed(1)),
        Instr::set_local(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        num_locals: 1,
        number_literals: vec![4.0],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test32() {
    // When the last argument is a function call, all its return values are passed
    let text = "local type, print print(type(nil))";
    let code = vec![
        Instr::push_nil(),
        Instr::push_nil(),
        Instr::set_local(1),
        Instr::set_local(0),
        // The callee is evaluated first, then the base is marked one slot
        // below the top. Marking before evaluation required guessing how many
        // values the callee expression had already left on the stack.
        Instr::get_local(1),                                // Get print
        Instr::mark_call_base(1),                           // Mark the slot holding the callee
        Instr::get_local(0),                                // Get type
        Instr::nop(),                                       // Inner fixed call's retired mark
        Instr::push_nil(),                                  // Push nil argument
        Instr::call(ArgCount::Fixed(1), RetCount::All),     // Call type(nil), return ALL values
        Instr::call(ArgCount::Dynamic, RetCount::Fixed(0)), // Call print with dynamic arg count
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        num_locals: 2,
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn test33() {
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
fn break_allows_semicolons_and_following_statements() {
    for source in [
        "while true do break; end",
        "while true do break;;; end",
        "while true do break; reached = 1 end",
    ] {
        parse_unfinalized(source).expect("break must allow following statements");
    }

    let mut state = State::new();
    crate::Program::compile(
        "local reached = 0; while true do break; reached = 1 end; return reached",
        None,
    )
    .and_then(|program| state.load(&program))
    .expect("break fixture must compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("break fixture must run");
    assert_eq!(
        state.to_number(-1).expect("return value must be numeric"),
        0.0
    );
}

#[test]
fn test34() {
    // while false do local b end - an uncaptured body local needs no close
    let text = "while false do local b end b()";
    let code = vec![
        Instr::push_bool(false),
        Instr::branch_false(3),
        Instr::push_nil(),
        Instr::set_local(0),
        Instr::jump(-5),
        Instr::get_global(0, None),
        Instr::nop(),
        Instr::call(ArgCount::Fixed(0), RetCount::Fixed(0)),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let chunk = Bytecode {
        code,
        num_locals: 1,
        string_literals: vec!["b".into()],
        ..Bytecode::default()
    };
    check_it(text, chunk);
}

#[test]
fn assignment_tail_vararg_expands_to_lvalue_count() {
    let chunk = parse_unfinalized("f = function(...) a, b = ... end").unwrap();
    let function = &chunk.nested[0];

    assert_eq!(
        function.code,
        vec![
            Instr::vararg(RetCount::Fixed(2)),
            Instr::set_global(1, None),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ]
    );
}

#[test]
fn generic_for_tail_vararg_expands_to_iterator_triple() {
    let chunk = parse_unfinalized("f = function(...) for x in ... do end end").unwrap();
    let function = &chunk.nested[0];

    assert_eq!(
        function
            .code
            .iter()
            .filter(|instr| instr.opcode() == Instr::OP_VARARG)
            .map(|instr| instr.a())
            .collect::<Vec<_>>(),
        vec![3]
    );
}

#[test]
fn non_final_vararg_stays_single_valued() {
    let chunk = parse_unfinalized("f = function(...) a, b, c = ..., 99 end").unwrap();
    let function = &chunk.nested[0];

    assert_eq!(
        function
            .code
            .iter()
            .filter(|instr| instr.opcode() == Instr::OP_VARARG)
            .map(|instr| instr.a())
            .collect::<Vec<_>>(),
        vec![1]
    );
}

#[test]
fn local_vararg_assignment_bytecode_is_unchanged() {
    let chunk = parse_unfinalized("f = function(...) local a, b, c = ... end").unwrap();
    let function = &chunk.nested[0];

    assert_eq!(
        function.code,
        vec![
            Instr::vararg(RetCount::Fixed(3)),
            Instr::set_local(2),
            Instr::set_local(1),
            Instr::set_local(0),
            Instr::ret(RetCount::Fixed(0)),
        ]
    );
}

#[test]
fn local_tail_call_assignment_bytecode_is_unchanged() {
    let chunk = parse_unfinalized("f = function() local a, b, c = g() end").unwrap();
    let function = &chunk.nested[0];

    assert_eq!(
        function.code,
        vec![
            Instr::get_global(0, None),
            Instr::nop(),
            Instr::call(ArgCount::Fixed(0), RetCount::Fixed(3)),
            Instr::set_local(2),
            Instr::set_local(1),
            Instr::set_local(0),
            Instr::ret(RetCount::Fixed(0)),
        ]
    );
}

#[test]
fn parameterized_function_tracks_only_non_parameter_locals() {
    let chunk = parse_unfinalized("function f(a, b) do local x end do local y end end").unwrap();
    let function = &chunk.nested[0];

    assert_eq!(function.num_params, 2);
    assert_eq!(function.num_locals, 1);
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

        assert_eq!(
            function.line_info,
            vec![2, 2, 5, 5, 5, 6, 6, 6, 6, 6, 6, 7],
            "{newline:?}"
        );
        let call_lines: Vec<_> = function
            .code
            .iter()
            .zip(&function.line_info)
            .filter_map(|(instruction, line)| {
                (instruction.opcode() == Instr::OP_CALL).then_some(*line)
            })
            .collect();
        assert_eq!(call_lines, vec![5, 6], "{newline:?}");
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
            Instr::closure(0),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ]
    );
    assert_eq!(chunk.string_literals, vec![Vec::from("f")]);

    let f = &chunk.nested[0];
    assert_eq!(f.source.as_deref(), Some(&b"nested-lines.lua"[..]));
    assert_eq!(
        f.code,
        vec![
            Instr::closure(0),
            Instr::set_global(0, None),
            Instr::ret(RetCount::Fixed(0)),
        ]
    );
    assert_eq!(f.string_literals, vec![Vec::from("g")]);

    let g = &f.nested[0];
    assert_eq!(g.source.as_deref(), Some(&b"nested-lines.lua"[..]));
    assert_eq!(g.code, vec![Instr::ret(RetCount::Fixed(0))]);
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
fn assignment_offset_limit_matches_its_error_message() {
    let targets = vec!["t.k"; MAX_ASSIGNMENT_OFFSET + 1].join(", ");
    parse_unfinalized(&format!("{targets} = 1")).expect("the maximum receiver offset must compile");

    let targets = vec!["t.k"; MAX_ASSIGNMENT_OFFSET + 2].join(", ");
    let error = parse_unfinalized(&format!("{targets} = 1"))
        .expect_err("an offset past MAX_ASSIGNMENT_OFFSET must fail");
    assert!(matches!(
        error.kind(),
        ErrorKind::SyntaxError(SyntaxError::AssignmentOffsetTooLarge)
    ));
    assert!(
        error
            .to_string()
            .contains(&format!("{MAX_ASSIGNMENT_OFFSET}"))
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
    assert!(
        bytecode
            .code
            .iter()
            .any(|instruction| instruction.opcode() == Instr::OP_NEW_TABLE_PRESIZED)
    );
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
    assert!(
        bytecode
            .code
            .iter()
            .any(|instruction| instruction.opcode() == Instr::OP_NEW_TABLE_PRESIZED)
    );
}

#[test]
fn table_array_batch_limit_splits_at_the_encoded_count() {
    let source = format!("t = {{{}}}", value_list(usize::from(MAX_TABLE_BATCH_SIZE)));
    let bytecode = parse_unfinalized(&source).expect("one full array batch must compile");
    let set_lists = bytecode
        .code
        .iter()
        .filter(|instruction| instruction.opcode() == Instr::OP_SET_LIST)
        .collect::<Vec<_>>();
    assert_eq!(set_lists.len(), 1);
    assert_eq!(set_lists[0].a(), MAX_TABLE_BATCH_SIZE);
    assert_eq!(set_lists[0].bx(), 0);

    let source = format!(
        "t = {{{}}}",
        value_list(usize::from(MAX_TABLE_BATCH_SIZE) + 1)
    );
    let bytecode = parse_unfinalized(&source).expect("one more entry must begin the next batch");
    let set_lists = bytecode
        .code
        .iter()
        .filter(|instruction| instruction.opcode() == Instr::OP_SET_LIST)
        .collect::<Vec<_>>();
    assert_eq!(set_lists.len(), 2);
    assert_eq!(
        (set_lists[0].a(), set_lists[0].bx()),
        (MAX_TABLE_BATCH_SIZE, 0)
    );
    assert_eq!((set_lists[1].a(), set_lists[1].bx()), (1, 1));
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
    // bind to the enclosing function's loop (it once emitted a jump into the
    // nested chunk and then patched the outer chunk at that index).
    let loops = [
        ("while true do ", " end"),
        ("repeat ", " until true"),
        ("for i = 1, 3 do ", " end"),
        ("for k, v in pairs({}) do ", " end"),
    ];
    let bodies = [
        "local f = function() break end",
        // Long inner body: the nested jump index lies past the outer chunk.
        "local f = function() local a = 1 local b = 2 local c = 3 local d = 4 \
         local e = 5 local g = 6 local h = 7 break end",
        // Break nested in a loop-free block of the inner function.
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
    // A loop in the nested function and a loop around it each patch only
    // their own breaks.
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
    //
    // Statement nesting costs only ~3 small native frames per depth tick, so on
    // its own it proves very little.
    let depth = 190;
    parse_unfinalized(&format!("{}{}", "do ".repeat(depth), "end ".repeat(depth)))
        .expect("statement nesting below the limit must compile");

    // Nested parens are the worst case: each level descends through both the
    // parse_expr and parse_unary wrappers plus the whole precedence ladder, so
    // it burns ~13 native frames per level and charges 2 depth ticks. Two ticks
    // per paren is why `local x = (...(1)...)` tops out at 98 levels rather
    // than 200; 95 is the deepest round number that still compiles.
    let paren_depth = 95;
    parse_unfinalized(&format!(
        "local x = {}1{}",
        "(".repeat(paren_depth),
        ")".repeat(paren_depth)
    ))
    .expect("paren nesting below the limit must compile");

    // Nested call arguments and indexes retain additional suffix-parser
    // frames. Their active contents charge SYNTAX_SUFFIX_COST extra ticks to
    // keep native-stack usage per budget tick below the parenthesis cycle
    // above; 63 levels at three ticks per level overflowed a 2MB thread.
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

fn assert_no_opcode(source: &str, opcode: u8) {
    let chunk = parse_unfinalized(source).expect("fold fixture compiles");
    assert!(
        chunk.code.iter().all(|instr| instr.opcode() != opcode),
        "expected opcode {opcode} folded away in `{source}`, got {:?}",
        chunk.code
    );
}

fn assert_has_opcode(source: &str, opcode: u8) {
    let chunk = parse_unfinalized(source).expect("fold fixture compiles");
    assert!(
        chunk.code.iter().any(|instr| instr.opcode() == opcode),
        "expected opcode {opcode} NOT folded in `{source}`, got {:?}",
        chunk.code
    );
}

fn eval_number(source: &str) -> f64 {
    let mut state = State::new();
    crate::Program::compile(source, None)
        .and_then(|program| state.load(&program))
        .expect("fold fixture compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("fold fixture runs");
    state.to_number(-1).expect("fold fixture returns a number")
}

#[test]
fn constant_folding_collapses_literal_arithmetic() {
    assert_no_opcode("return 60 * 1000", Instr::OP_MULTIPLY);
    assert_no_opcode("return 1 + 2 + 3", Instr::OP_ADD);
    assert_no_opcode("return 10 - 4", Instr::OP_SUBTRACT);
    assert_no_opcode("return 10 / 4", Instr::OP_DIVIDE);
    assert_no_opcode("return 10 % 3", Instr::OP_MOD);
    // `^` is never folded: runtime `^` is charged, a folded literal would not be.
    assert_has_opcode("return 2 ^ 10", Instr::OP_POW);
    assert_has_opcode("return 2 ^ 0.5 + 1", Instr::OP_ADD);
    assert_no_opcode("return -5", Instr::OP_NEGATE);
    assert_no_opcode("return -(2 * 3)", Instr::OP_NEGATE);

    let chunk = parse_unfinalized("return 60 * 1000 + 8").expect("chain folds");
    assert!(chunk.number_literals.contains(&60008.0));

    assert_eq!(eval_number("return 60 * 1000 + 8"), 60008.0);
    assert_eq!(eval_number("return -5 % 3"), 1.0);
    assert_eq!(eval_number("return 5 % -3"), -1.0);
    assert_eq!(eval_number("return 2 ^ 10"), 1024.0);
    assert_eq!(eval_number("return - -5"), 5.0);
}

#[test]
fn constant_folding_refuses_nan_and_negative_zero_results() {
    // NaN payloads and -0.0 collapse in the ==-deduped literal pool, so
    // these stay runtime operations (reference lcode.c's guards).
    assert_has_opcode("return 0 / 0", Instr::OP_DIVIDE);
    assert_has_opcode("return -0", Instr::OP_NEGATE);
    assert_has_opcode("return 0 * -1", Instr::OP_MULTIPLY);
    // Infinity is a normal foldable value.
    assert_no_opcode("return 1 / 0", Instr::OP_DIVIDE);
    assert_eq!(eval_number("return 1 / 0"), f64::INFINITY);
}

#[test]
fn constant_folding_only_fires_on_literal_operands() {
    assert_has_opcode("local x = 1 return x + 2", Instr::OP_ADD);
    assert_has_opcode("local x = 1 return 2 + x", Instr::OP_ADD);
    // Mixed: the literal half folds, the variable half stays.
    let chunk = parse_unfinalized("local x = 1 return x + 2 * 3").expect("mixed compiles");
    assert!(chunk.code.iter().any(|i| i.opcode() == Instr::OP_ADD));
    assert!(chunk.code.iter().all(|i| i.opcode() != Instr::OP_MULTIPLY));
}

#[test]
fn folding_and_fusion_keep_line_info_synced_with_code() {
    // Folding pops both arrays and fusion rewrites in place; a desync here
    // corrupts stack traces silently.
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
fn compare_branch_fusion_shapes_and_semantics() {
    // Conditions ending in a comparison fuse with the branch in while, if,
    // and repeat-until forms.
    assert_no_opcode("local n = 0 while n < 3 do n = n + 1 end", Instr::OP_LESS);
    assert_no_opcode(
        "local a = 1 if a >= 2 then return 1 end return 0",
        Instr::OP_GREATER_EQUAL,
    );
    assert_no_opcode(
        "local n = 0 repeat n = n + 1 until n ~= 1",
        Instr::OP_NOT_EQUAL,
    );

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
    // NaN compares false through the fused path, like the push path.
    assert_eq!(
        eval_number("local nan = 0/0 if nan == nan then return 1 else return 2 end"),
        2.0
    );
    // The fused path still raises the comparison type error.
    let mut state = State::new();
    crate::Program::compile("if 1 < 'x' then return 1 end return 0", None)
        .and_then(|program| state.load(&program))
        .expect("type-error fixture compiles");
    assert!(
        state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).is_err(),
        "number/string comparison must still error when fused"
    );
}

#[test]
fn compare_branch_fusion_respects_or_skip_targets() {
    // `a or (1 < 2)`: the or-skip lands on the would-be branch slot, which
    // must pop exactly one condition value - fusing would pop two.
    let source = "local a = 5 if a or 1 < 2 then return 1 end return 0";
    assert_has_opcode(source, Instr::OP_LESS);
    assert_eq!(eval_number(source), 1.0);
    assert_eq!(
        eval_number("local a = false if a or 2 < 1 then return 1 end return 0"),
        0.0
    );
    assert_eq!(
        eval_number("local a = false if a or 1 < 2 then return 1 end return 0"),
        1.0
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
        assert_no_opcode(source, Instr::OP_JUMP);
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
    // The outer arm's last instruction is the nested `return 2`, but the
    // nested `if`'s false branch lands past it and must still jump over the
    // outer `else`.
    let source = "local a, b, r = true, false, 0 \
                  if a then if b then return 1 elseif b == nil then return 2 end \
                  else r = 7 end return r + 3";
    assert_has_opcode(source, Instr::OP_JUMP);
    assert_eq!(eval_number(source), 3.0);
    let source = "local a, b = true, false \
                  if a then if b then return 1 end else return 7 end return 3";
    assert_eq!(eval_number(source), 3.0);
}

#[test]
fn if_arm_does_not_inherit_a_do_blocks_return_status() {
    // A direct return in a do-block belongs to that block. The outer arm
    // conservatively keeps its jump, even though this particular arm returns.
    let source = "local a = true if a then do return 1 end else return 7 end return 3";
    assert_has_opcode(source, Instr::OP_JUMP);
    assert_eq!(eval_number(source), 1.0);

    // A conditional return inside the do-block can fall through. Neither the
    // block nor a later statement may make the outer arm skip its end jump.
    for tail in ["", "r = 2"] {
        let source = format!(
            "local a, b, r = true, false, 0 \
             if a then do if b then return 1 end end {tail} \
             else r = 7 end return r + 3"
        );
        assert_has_opcode(&source, Instr::OP_JUMP);
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
fn constant_folding_respects_jump_targets() {
    // The `or` skip branch lands on the second operand push; merging the
    // pair would make a truthy `a` skip the multiplication entirely.
    assert_has_opcode("local a = 5 return (a or 2) * 3", Instr::OP_MULTIPLY);
    assert_eq!(eval_number("local a = 5 return (a or 2) * 3"), 15.0);
    assert_eq!(eval_number("local a = false return (a or 2) * 3"), 6.0);
    assert_eq!(eval_number("local a = 5 return (a and 2) * 3"), 6.0);

    // A while condition is a backward-jump target; landing on the folded
    // push is value-equivalent, so this may fold - and must still loop.
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
fn return_255_literals_uses_explicit_all_encoding() {
    let source = format!("return {}", vec!["1"; 255].join(","));
    let bytecode = parse_unfinalized(&source).expect("255 literal returns remain accepted");
    assert_eq!(bytecode.code[255], Instr::ret(RetCount::All));
    assert_eq!(
        bytecode
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_PUSH_NUM)
            .count(),
        255
    );
}

#[test]
fn nested_dynamic_call_arguments_survive_tail_rewrites() {
    for source in [
        "local function f(...) return ... end; return f(f(f(1, 2, 3)))",
        "local t = {}; function t:f(...) return ... end; return t:f(t:f(t:f(1, 2, 3)))",
    ] {
        let bytecode = parse_unfinalized(source).expect("nested dynamic calls compile");
        assert!(bytecode.code.iter().any(|inst| {
            inst.opcode() == Instr::OP_CALL
                && inst.call_args() == ArgCount::Dynamic
                && inst.call_results() == RetCount::All
        }));
    }
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
fn block_exit_restores_shadowed_capture_slot_without_closing_parent_on_function_exit() {
    let chunk = parse_unfinalized(
        "local x; do local x; first = function() return x end end; \
         second = function() return x end",
    )
    .expect("shadowed capture fixture must compile");
    assert_eq!(chunk.nested[0].upvalues, vec![UpvalueDesc::Local(1)]);
    assert_eq!(chunk.nested[1].upvalues, vec![UpvalueDesc::Local(0)]);
    assert_eq!(
        chunk.code,
        vec![
            Instr::push_nil(),
            Instr::set_local(0),
            Instr::push_nil(),
            Instr::set_local(1),
            Instr::closure(0),
            Instr::set_global(0, None),
            Instr::close_upvalues(1),
            Instr::closure(1),
            Instr::set_global(1, None),
            Instr::ret(RetCount::Fixed(0)),
        ]
    );
}
