use super::{Instr, parse_unfinalized};

#[test]
fn multiline_operator_line_info_golden() {
    // Source-level attribution: both additions used to inherit line 1.
    // Lua's operator-line correction assigns the two '+' tokens lines 1 and 2.
    let chunk = parse_unfinalized("return a +\n b +\n c").expect("golden source compiles");
    assert_eq!(chunk.line_info, vec![1, 2, 1, 3, 2, 3, 3]);
}

#[test]
fn operator_lines_survive_parentheses_unary_and_concat() {
    for (source, opcode, expected) in [
        ("return (a\n) +\n b", Instr::OP_ADD, vec![2]),
        ("return a\n *\n b /\n c %\n d", Instr::OP_MULTIPLY, vec![2]),
        ("return a\n <\n b", Instr::OP_LESS, vec![2]),
        ("return -\n a", Instr::OP_NEGATE, vec![1]),
        ("return #\n a", Instr::OP_LENGTH, vec![1]),
        ("return a ^\n b ^\n c", Instr::OP_POW, vec![2, 1]),
        ("return a ..\n b ..\n c", Instr::OP_CONCAT, vec![1]),
    ] {
        let chunk = parse_unfinalized(source).expect("operator source compiles");
        let lines: Vec<_> = chunk
            .code
            .iter()
            .zip(&chunk.line_info)
            .filter_map(|(instruction, line)| (instruction.opcode() == opcode).then_some(*line))
            .collect();
        assert_eq!(lines, expected, "{source:?}");
    }
}

#[test]
fn expression_line_breaks_preserve_instructions_and_literals() {
    for source in [
        "return a +\n b *\n c",
        "return (a\n) +\n b",
        "return -\n a ^\n b",
        "return a ..\n b ..\n c",
        "return 1 +\n 2 *\n 3",
        "return f(\n a,\n b)",
    ] {
        let multiline = parse_unfinalized(source).expect("multiline source compiles");
        let singleline =
            parse_unfinalized(&source.replace('\n', " ")).expect("singleline source compiles");
        assert_eq!(multiline.code, singleline.code, "{source:?}");
        assert_eq!(multiline.number_literals, singleline.number_literals);
        assert_eq!(multiline.string_literals, singleline.string_literals);
    }
}

#[test]
fn runtime_arithmetic_error_uses_operator_line() {
    for (source, expected) in [
        ("return 1 +\n nil", 1),
        ("return 1 +\n 2 +\n nil", 2),
        ("return (1\n) +\n nil", 2),
    ] {
        let mut state = crate::State::new();
        crate::Program::compile(source, None)
            .and_then(|program| state.load(&program))
            .expect("runtime fixture compiles");
        let error = state
            .call(crate::ArgCount::Fixed(0), crate::RetCount::Fixed(1))
            .expect_err("nil arithmetic must fail");
        assert_eq!(
            error.stack_trace().first().expect("error has a frame").line,
            expected
        );
    }
}
