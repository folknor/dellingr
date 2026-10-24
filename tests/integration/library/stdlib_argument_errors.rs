//! Standard-library argument validation: integer-argument exactness, range
//! errors on type-correct values, and the literal-pattern fast path.

use dellingr::error::{ArgDetail, ErrorKind, ErrorValue, PrefixLocation};
use dellingr::{ArgCount, RetCount, State};

use crate::support;

fn argument_kind(code: &str) -> ErrorKind {
    let mut state = State::new();
    support::load(&mut state, code);
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("regression script must fail");
    error.into_kind()
}

fn argument_message(code: &str) -> String {
    match argument_kind(code) {
        ErrorKind::ArgError(argument) => argument.to_string(),
        kind => panic!("expected a structured argument error, got {kind:?}"),
    }
}

#[test]
fn string_find_keeps_the_reference_literal_close_parenthesis_case() {
    let mut state = State::new();
    support::eval(
        &mut state,
        r#"local first, last = string.find("a)", ")"); return first == 2 and last == 2"#,
        1,
    );
    assert!(state.to_boolean(-1));
}

#[test]
fn range_and_union_argument_errors_name_the_invalid_value() {
    assert_eq!(
        argument_message(r#"return tonumber("10", 37)"#),
        "bad argument #2 to 'tonumber' (base out of range)"
    );
    assert_eq!(
        argument_message(r#"return select(0, "a")"#),
        "bad argument #1 to 'select' (index out of range)"
    );
    assert_eq!(
        argument_message(r#"return select(-2, "a")"#),
        "bad argument #1 to 'select' (index out of range)"
    );
    assert_eq!(
        argument_message("return rawlen(1)"),
        "bad argument #1 to 'rawlen' (table or string expected)"
    );
}

#[test]
fn error_nil_level_uses_the_default_and_preserves_the_script_message() {
    let mut state = State::new();
    support::load(&mut state, r#"error("original message", nil)"#);
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("error raises the supplied message");
    let ErrorKind::ScriptError { value, prefix } = error.kind() else {
        panic!("expected the script error to be preserved, got {error}");
    };
    assert_eq!(value, &ErrorValue::String(b"original message".to_vec()));
    assert!(matches!(prefix, PrefixLocation::TraceFrame(0)));
}

#[test]
fn stdlib_integer_arguments_reject_fractional_numbers_consistently() {
    let cases: [(&str, &str, isize); 15] = [
        (r#"return unpack({}, 1.5)"#, "unpack", 2),
        (r#"return table.insert({}, 1.5, "x")"#, "insert", 2),
        (r#"return table.remove({}, 1.5)"#, "remove", 2),
        (r#"return table.concat({}, "", 1.5)"#, "concat", 3),
        (r#"return table.move({}, 1.5, 1, 1)"#, "move", 2),
        (r#"return string.sub("abc", 1.5)"#, "sub", 2),
        (r#"return string.find("abc", "a", 1.5)"#, "find", 3),
        (r#"return string.match("abc", "a", 1.5)"#, "match", 3),
        (r#"return string.gmatch("abc", "a", 1.5)"#, "gmatch", 3),
        (r#"return string.gsub("a", "a", "b", 1.5)"#, "gsub", 4),
        (r#"return string.format("%d", 1.5)"#, "format", 2),
        (r#"return math.random(1.5)"#, "random", 1),
        (r#"return select(1.5, "a")"#, "select", 1),
        (r#"return tonumber("10", 2.5)"#, "tonumber", 2),
        (r#"error("x", 1.5)"#, "error", 2),
    ];

    for (code, function, argument_number) in cases {
        let kind = argument_kind(code);
        let ErrorKind::ArgError(argument) = kind else {
            panic!("expected an argument error for {code}, got {kind:?}");
        };
        assert_eq!(
            argument.detail(),
            &ArgDetail::NoInteger,
            "wrong argument error for {code}"
        );
        assert_eq!(argument.func_name(), Some(function), "function for {code}");
        assert_eq!(
            argument.arg_number(),
            argument_number,
            "argument position for {code}"
        );
    }
}

#[test]
fn argument_details_are_structured_and_rendered_as_expected() {
    use dellingr::error::ArgDetail;

    for (code, expected_detail, expected_text) in [
        (
            "return tonumber('10', 37)",
            ArgDetail::BaseOutOfRange,
            "bad argument #2 to 'tonumber' (base out of range)",
        ),
        (
            "return select(0, 'a')",
            ArgDetail::IndexOutOfRange,
            "bad argument #1 to 'select' (index out of range)",
        ),
        (
            "return rawlen(1)",
            ArgDetail::TableOrStringExpected,
            "bad argument #1 to 'rawlen' (table or string expected)",
        ),
        (
            "return math.random(2, 1)",
            ArgDetail::IntervalEmpty,
            "bad argument #1 to 'random' (interval is empty)",
        ),
        (
            "return string.sub('a', 1.5)",
            ArgDetail::NoInteger,
            "bad argument #2 to 'sub' (number has no integer representation)",
        ),
    ] {
        let mut state = State::new();
        support::load(&mut state, code);
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("invalid argument fails");
        let ErrorKind::ArgError(argument) = error.kind() else {
            panic!("expected argument error for {code}");
        };
        assert_eq!(argument.detail(), &expected_detail, "{code}");
        assert_eq!(argument.to_string(), expected_text, "{code}");
    }
}
