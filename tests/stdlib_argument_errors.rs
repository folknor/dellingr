//! Standard-library argument validation: integer-argument exactness, range
//! errors on type-correct values, and the literal-pattern fast path.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

fn runtime_message(code: &str) -> String {
    let mut state = State::new();
    state.load_string(code).expect("regression script compiles");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("regression script must fail");
    match error.kind {
        ErrorKind::RuntimeError(message) => message,
        kind => panic!("expected a runtime argument error, got {kind:?}"),
    }
}

#[test]
fn string_find_keeps_the_reference_literal_close_parenthesis_case() {
    let mut state = State::new();
    state
        .load_string(
            r#"local first, last = string.find("a)", ")"); return first == 2 and last == 2"#,
        )
        .expect("regression script compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("literal close parenthesis search succeeds");
    assert!(state.to_boolean(-1));
}

#[test]
fn range_and_union_argument_errors_name_the_invalid_value() {
    assert_eq!(
        runtime_message(r#"return tonumber("10", 37)"#),
        "bad argument #2 to 'tonumber' (base out of range)"
    );
    assert_eq!(
        runtime_message(r#"return select(0, "a")"#),
        "bad argument #1 to 'select' (index out of range)"
    );
    assert_eq!(
        runtime_message(r#"return select(-2, "a")"#),
        "bad argument #1 to 'select' (index out of range)"
    );
    assert_eq!(
        runtime_message("return rawlen(1)"),
        "bad argument #1 to 'rawlen' (table or string expected)"
    );
}

#[test]
fn stdlib_integer_arguments_reject_fractional_numbers_consistently() {
    let cases = [
        r#"return unpack({}, 1.5)"#,
        r#"return table.insert({}, 1.5, "x")"#,
        r#"return table.remove({}, 1.5)"#,
        r#"return table.concat({}, "", 1.5)"#,
        r#"return table.move({}, 1.5, 1, 1)"#,
        r#"return string.sub("abc", 1.5)"#,
        r#"return string.find("abc", "a", 1.5)"#,
        r#"return string.match("abc", "a", 1.5)"#,
        r#"return string.gsub("a", "a", "b", 1.5)"#,
        r#"return string.format("%d", 1.5)"#,
        r#"return math.random(1.5)"#,
        r#"return select(1.5, "a")"#,
        r#"return tonumber("10", 2.5)"#,
        r#"error("x", 1.5)"#,
    ];

    for code in cases {
        let message = runtime_message(code);
        assert!(
            message.contains("(number has no integer representation)"),
            "wrong argument error for {code}: {message}"
        );
    }
}
