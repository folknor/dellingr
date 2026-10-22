use super::parse_unfinalized;
use crate::instr::{ArithOp, CompareOp, Instr};

fn lines_where(source: &str, matches: impl Fn(u8) -> bool) -> Vec<u32> {
    let chunk = parse_unfinalized(source).expect("operator source compiles");
    chunk
        .code
        .iter()
        .zip(&chunk.line_info)
        .filter_map(|(instruction, line)| matches(instruction.opcode()).then_some(*line))
        .collect()
}

fn arith(op: ArithOp) -> impl Fn(u8) -> bool {
    move |opcode| ArithOp::decode(opcode).is_some_and(|(found, _)| found == op)
}

#[test]
fn multiline_operator_line_info_golden() {
    // Lua's operator-line correction assigns the two '+' tokens lines 1 and 2,
    // not the line their right operand ends on.
    assert_eq!(
        lines_where("return a +\n b +\n c", arith(ArithOp::Add)),
        vec![1, 2]
    );
}

#[test]
fn operator_lines_survive_parentheses_unary_and_concat() {
    assert_eq!(
        lines_where("return (a\n) +\n b", arith(ArithOp::Add)),
        vec![2]
    );
    assert_eq!(
        lines_where("return a\n *\n b /\n c %\n d", arith(ArithOp::Mul)),
        vec![2]
    );
    assert_eq!(
        lines_where("return a\n <\n b", |op| op == CompareOp::Lt.value_opcode()),
        vec![2]
    );
    assert_eq!(
        lines_where("return -\n a", |op| op == Instr::OP_UNM),
        vec![1]
    );
    assert_eq!(
        lines_where("return #\n a", |op| op == Instr::OP_LEN),
        vec![1]
    );
    assert_eq!(
        lines_where("return a ^\n b ^\n c", arith(ArithOp::Pow)),
        vec![2, 1]
    );
    assert_eq!(
        lines_where("return a ..\n b ..\n c", |op| op == Instr::OP_CONCAT),
        vec![1]
    );
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
