use std::sync::Arc;

use super::Bytecode;
use super::Instr;
use super::Parser;
use super::TokenStream;
use super::parse_str;
use crate::State;
use crate::compiler::UpvalueDesc;
use crate::error::{ErrorKind, SyntaxError};
use crate::instr::{ArgCount, Builtin, RetCount};

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
    let err = parse_str(source).expect_err("source must exceed the syntax depth limit");
    assert!(
        matches!(
            err.kind,
            ErrorKind::SyntaxError(SyntaxError::TooManySyntaxLevels),
        ),
        "unexpected error: {err:?}"
    );
}

fn check_it(input: &str, mut output: Bytecode) {
    // Top-level chunks are always vararg functions
    output.is_vararg = true;
    let mut actual = parse_str(input).unwrap();
    // Clear line_info for comparison (tests were written before line tracking existed)
    clear_line_info(&mut actual);
    assert_eq!(actual, output);
}

fn literal_bytes(source: &str) -> Vec<u8> {
    let chunk = parse_str(source).expect("test string literal should compile");
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
        let err = parse_str(source).expect_err("malformed escape must fail");
        assert!(
            matches!(
                (&err.kind, expected),
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
            err.column,
            source.find('\\').expect("source has escape") + 1
        );
    }
}

#[test]
fn checked_jump_offset_accepts_i16_boundaries_only() {
    let parser = Parser {
        input: TokenStream::new(""),
        chunk: Bytecode::default(),
        nest_level: 0,
        locals: Vec::new(),
        outer_chunks: Vec::new(),
        loop_breaks: Vec::new(),
        upvalues: Vec::new(),
        outer_locals: Vec::new(),
        outer_upvalues: Vec::new(),
        current_line: 1,
        syntax_depth: 0,
    };

    assert_eq!(parser.checked_jump_offset(0, 32_768).unwrap(), i16::MAX);
    assert!(parser.checked_jump_offset(0, 32_769).is_err());
    assert_eq!(parser.checked_jump_offset(32_767, 0).unwrap(), i16::MIN);
    assert!(parser.checked_jump_offset(32_768, 0).is_err());
}

#[test]
fn test01() {
    let text = "x = 5 + 6";
    let out = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::push_num(1),
            Instr::add(),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![5.0, 6.0],
        string_literals: vec!["x".into()],
        ..Bytecode::default()
    };
    check_it(text, out);
}

#[test]
fn test02() {
    let text = "x = -5^2";
    let out = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::push_num(1),
            Instr::pow(),
            Instr::negate(),
            Instr::set_global(0),
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
            Instr::set_global(0),
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
    let text = "x = 1 .. 2 + 3";
    let output = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::push_num(1),
            Instr::push_num(2),
            Instr::add(),
            Instr::concat(2),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![1.0, 2.0, 3.0],
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
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        string_literals: vec!["x".into(), "a".into(), "b".into(), "c".into(), "d".into()],
        ..Bytecode::default()
    };
    check_it(text, output);
}

#[test]
fn test05() {
    let text = "x = 2^-3";
    let output = Bytecode {
        code: vec![
            Instr::push_num(0),
            Instr::push_num(1),
            Instr::negate(),
            Instr::pow(),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ],
        number_literals: vec![2.0, 3.0],
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
            Instr::set_global(0),
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
            Instr::set_global(0),
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
            Instr::set_global(0),
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
        Instr::set_global(0),
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
        Instr::set_global(0),
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
        Instr::set_global(0),
        Instr::push_bool(true),
        Instr::branch_false(2),
        Instr::push_num(1),
        Instr::set_global(1),
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
        Instr::set_global(0),
        Instr::jump(2),
        Instr::push_num(1),
        Instr::set_global(0),
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
    let text = "if true then a = 5 elseif 6 == 7 then a = 3 else a = 4 end";
    let code = vec![
        Instr::push_bool(true),
        Instr::branch_false(3),
        Instr::push_num(0),
        Instr::set_global(0),
        Instr::jump(9),
        Instr::push_num(1),
        Instr::push_num(2),
        Instr::equal(),
        Instr::branch_false(3),
        Instr::push_num(3),
        Instr::set_global(0),
        Instr::jump(2),
        Instr::push_num(4),
        Instr::set_global(0),
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
    let text = "while a < 10 do a = a + 1 end";
    let code = vec![
        Instr::get_global(0),
        Instr::push_num(0),
        Instr::less(),
        Instr::branch_false(5),
        Instr::get_global(0),
        Instr::push_num(1),
        Instr::add(),
        Instr::set_global(0),
        Instr::jump(-9),
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
    // repeat local x = 5 until a == b - body locals get CloseUpvalues before jump
    let text = "repeat local x = 5 until a == b y = 4";
    let code = vec![
        Instr::push_num(0),
        Instr::set_local(0),
        Instr::get_global(0),
        Instr::get_global(1),
        Instr::equal(),
        Instr::close_upvalues(0), // close body-local x before potential jump back
        Instr::branch_false(-7),
        Instr::close_upvalues(0),
        Instr::push_num(1),
        Instr::set_global(2),
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
        Instr::set_global(0),
        Instr::close_upvalues(1),
        Instr::get_local(0),
        Instr::set_global(0),
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
        Instr::set_global(0),
        Instr::close_upvalues(0),
        Instr::get_global(1),
        Instr::set_global(0),
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
        Instr::branch_false(4),
        Instr::push_nil(),
        Instr::set_local(1),
        Instr::close_upvalues(1),
        Instr::jump(2),
        Instr::get_local(0),
        Instr::set_global(0),
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
        Instr::for_prep(0, 4),
        Instr::get_local(3),
        Instr::set_global(0),
        Instr::close_upvalues(3),
        Instr::for_loop(0, -4),
        Instr::close_upvalues(0),
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
        Instr::get_global(0),
        Instr::call(ArgCount::Fixed(1), RetCount::Fixed(3)),
        Instr::tfor_prep(0),
        Instr::tfor_call(0, 2),
        Instr::tfor_loop(0, 4),
        Instr::get_local(3),
        Instr::set_global(1),
        Instr::close_upvalues(3),
        Instr::jump(-6),
        Instr::close_upvalues(0),
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
    let chunk = parse_str("local i = 10; for i = (function() return i end)(), 10 do end")
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
        Instr::set_global(1),
        Instr::set_global(0),
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
        Instr::set_global(1),
        Instr::set_global(0),
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
        Instr::set_global(1),
        Instr::set_global(0),
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
        Instr::get_global(0),
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
        Instr::set_global(0),
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
        Instr::set_global(0),
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
        Instr::set_global(0),
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
        Instr::get_global(5),
        Instr::push_num(4),
        Instr::init_index(0),
        Instr::set_global(0),
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
    let final_call = parse_str("y = {7, f()}").unwrap();
    assert!(
        final_call
            .code
            .iter()
            .any(|instr| instr == &Instr::new_table_tracked(2))
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

    let non_final_call = parse_str("y = {f(), 7}").unwrap();
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

    let final_vararg = parse_str("y = {99, ...}").unwrap();
    assert!(
        final_vararg
            .code
            .iter()
            .any(|instr| instr == &Instr::new_table_tracked(2))
    );
    assert!(
        final_vararg
            .code
            .iter()
            .any(|instr| instr == &Instr::vararg(u8::MAX))
    );
    assert!(
        final_vararg
            .code
            .iter()
            .any(|instr| instr == &Instr::set_list(0))
    );

    let non_final_vararg = parse_str("y = {..., 99}").unwrap();
    assert!(
        non_final_vararg
            .code
            .iter()
            .any(|instr| instr == &Instr::vararg(1))
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
    let keyed_last = parse_str("y = {f(), k = 7}").unwrap();
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

    let call_last = parse_str("y = {k = 7, f()}").unwrap();
    assert!(
        call_last
            .code
            .iter()
            .any(|instr| instr == &Instr::new_table_tracked(2))
    );
    assert!(
        call_last
            .code
            .iter()
            .any(|instr| instr == &Instr::call(ArgCount::Fixed(0), RetCount::All))
    );

    let dynamic_call = parse_str("y = {f(g())}").unwrap();
    assert!(
        dynamic_call
            .code
            .iter()
            .any(|instr| { instr == &Instr::call(ArgCount::Dynamic, RetCount::All) })
    );

    let capacity = parse_str("y = {1, 2, 3, 4, f()}").unwrap();
    assert!(
        capacity
            .code
            .iter()
            .any(|instr| instr == &Instr::new_table_tracked(5))
    );
}

#[test]
fn table_constructor_does_not_expand_non_bare_tails() {
    // A parenthesized call is truncated to one value, so it is not a bare tail
    // and must not be patched to multi-return or use a tracked constructor.
    let paren_call = parse_str("y = {(f())}").unwrap();
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
    let field_access = parse_str("y = {f().x}").unwrap();
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
    let paren_vararg = parse_str("y = {(...)}").unwrap();
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
            .any(|instr| instr == &Instr::vararg(1))
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
        Instr::get_global(0),
        Instr::get_field(1),
        Instr::get_field(2),
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
        Instr::set_global(0),
        Instr::ret(RetCount::Fixed(0)),
    ];
    let string_literals = vec!["x".into()];
    let nested = vec![Arc::new(Bytecode {
        code: vec![Instr::ret(RetCount::Fixed(0))],
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
        ..Bytecode::default()
    };
    let outer_chunk = Bytecode {
        code: vec![
            Instr::closure(0),
            Instr::set_global(0),
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
        ..Bytecode::default()
    };
    let y = Bytecode {
        code: vec![Instr::ret(RetCount::Fixed(0))],
        ..Bytecode::default()
    };
    let x = Bytecode {
        code: vec![
            Instr::closure(0),
            Instr::set_local(0),
            Instr::get_builtin(Builtin::Print),
            Instr::get_local(0),
            Instr::call(ArgCount::Fixed(1), RetCount::Fixed(0)),
            Instr::ret(RetCount::Fixed(0)),
        ],
        nested: vec![Arc::new(y)],
        num_locals: 1,
        ..Bytecode::default()
    };
    let outer_chunk = Bytecode {
        code: vec![
            Instr::closure(0),
            Instr::set_global(0),
            Instr::closure(1),
            Instr::set_global(1),
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
        Instr::mark_call_base(0), // Mark stack position before function (no adjustment, not a field access)
        Instr::get_local(1),      // Get print
        Instr::get_local(0),      // Get type
        Instr::push_nil(),        // Push nil argument
        Instr::call(ArgCount::Fixed(1), RetCount::All), // Call type(nil), return ALL values
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
    use super::*;
    for text in [
        "print()\n(foo)()\n",
        "print()\r(foo)()\r",
        "print()\r\n(foo)()\r\n",
    ] {
        match parse_str(text) {
            Err(Error {
                kind: ErrorKind::SyntaxError(SyntaxError::LParenLineStart),
                line_num,
                column,
                ..
            }) => {
                assert_eq!(line_num, 2, "{text:?}");
                assert_eq!(column, 1, "{text:?}");
            }
            _ => panic!("Should detect ambiguous function call because of linebreak: {text:?}"),
        }
    }
}

#[test]
fn break_allows_semicolons_and_following_statements() {
    for source in [
        "while true do break; end",
        "while true do break;;; end",
        "while true do break; reached = 1 end",
    ] {
        parse_str(source).expect("break must allow following statements");
    }

    let mut state = State::new();
    state
        .load_string("local reached = 0; while true do break; reached = 1 end; return reached")
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
    // while false do local b end - body locals get CloseUpvalues before jump
    let text = "while false do local b end b()";
    let code = vec![
        Instr::push_bool(false),
        Instr::branch_false(4),
        Instr::push_nil(),
        Instr::set_local(0),
        Instr::close_upvalues(0), // close body-local b before jump back
        Instr::jump(-6),
        Instr::close_upvalues(0),
        Instr::get_global(0),
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
    let chunk = parse_str("f = function(...) a, b = ... end").unwrap();
    let function = &chunk.nested[0];

    assert_eq!(
        function.code,
        vec![
            Instr::vararg(2),
            Instr::set_global(1),
            Instr::set_global(0),
            Instr::ret(RetCount::Fixed(0)),
        ]
    );
}

#[test]
fn generic_for_tail_vararg_expands_to_iterator_triple() {
    let chunk = parse_str("f = function(...) for x in ... do end end").unwrap();
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
    let chunk = parse_str("f = function(...) a, b, c = ..., 99 end").unwrap();
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
    let chunk = parse_str("f = function(...) local a, b, c = ... end").unwrap();
    let function = &chunk.nested[0];

    assert_eq!(
        function.code,
        vec![
            Instr::vararg(3),
            Instr::set_local(2),
            Instr::set_local(1),
            Instr::set_local(0),
            Instr::ret(RetCount::Fixed(0)),
        ]
    );
}

#[test]
fn local_tail_call_assignment_bytecode_is_unchanged() {
    let chunk = parse_str("f = function() local a, b, c = g() end").unwrap();
    let function = &chunk.nested[0];

    assert_eq!(
        function.code,
        vec![
            Instr::get_global(0),
            Instr::call(ArgCount::Fixed(0), RetCount::Fixed(3)),
            Instr::set_local(2),
            Instr::set_local(1),
            Instr::set_local(0),
            Instr::ret(RetCount::Fixed(0)),
        ]
    );
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
        let chunk = parse_str(source).expect("line-info fixture must compile");
        assert_line_info_matches_code_len(&chunk);
    }
}

#[test]
fn line_info_reports_correct_line() {
    let mut state = State::new();
    state
        .load_string("function f() end\nf(); f(); f(); f(); f(); f(); f(); f(); f(); f()\nf(); f(); f(); f(); f(); f(); f(); f(); f(); f()\n\nlocal x = nil; x()")
        .expect("line-info fixture must compile");
    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("calling nil must fail");
    assert_eq!(
        err.stack_trace
            .first()
            .expect("runtime error must carry a stack frame")
            .line,
        5
    );
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

#[test]
fn too_many_upvalues_errors() {
    let err = parse_str(&upvalue_program(200, 60)).expect_err("260 upvalues must be rejected");
    assert!(matches!(
        err.kind,
        ErrorKind::SyntaxError(SyntaxError::TooManyUpvalues)
    ));
    parse_str(&upvalue_program(200, 50)).expect("250 upvalues must remain supported");
}

#[test]
fn syntax_depth_limit() {
    let depth = 10_000;
    assert_too_many_syntax_levels(&format!("{}1{}", "(".repeat(depth), ")".repeat(depth)));
    assert_too_many_syntax_levels(&format!("{}{}", "do ".repeat(depth), "end ".repeat(depth)));
    assert_too_many_syntax_levels(&format!("local x = {}1", "- ".repeat(depth)));
    assert_too_many_syntax_levels(&format!(
        "local x = {}1{}",
        "{".repeat(depth),
        "}".repeat(depth)
    ));
    assert_too_many_syntax_levels(&format!(
        "if true then {}end",
        "elseif true then ".repeat(depth)
    ));
    assert_too_many_syntax_levels(&format!("a{}", ".b".repeat(depth)));
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
    parse_str(&format!("{}{}", "do ".repeat(depth), "end ".repeat(depth)))
        .expect("statement nesting below the limit must compile");

    // Nested parens are the worst case: each level descends through both the
    // parse_expr and parse_unary wrappers plus the whole precedence ladder, so
    // it burns ~13 native frames per level and charges 2 depth ticks. Two ticks
    // per paren is why the effective paren limit is ~99 rather than 200; 95 is
    // the deepest round number that still compiles.
    let paren_depth = 95;
    parse_str(&format!(
        "local x = {}1{}",
        "(".repeat(paren_depth),
        ")".repeat(paren_depth)
    ))
    .expect("paren nesting below the limit must compile");

    // elseif chains recurse on their own axis, bypassing parse_statements.
    parse_str(&format!(
        "if true then {}end",
        "elseif true then ".repeat(depth)
    ))
    .expect("elseif chaining below the limit must compile");
}
