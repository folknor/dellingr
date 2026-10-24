use dellingr::error::{Error, ErrorKind, MalformedPatternDetail, PatternErrorDetail, TypeError};
use dellingr::{LuaType, State};

use crate::support;

fn assert_invalid_zero_index(error: &Error) {
    assert!(matches!(
        error.kind(),
        ErrorKind::InvalidStackIndex { index: 0 }
    ));
}

#[test]
fn public_argument_checks_reject_zero_with_a_typed_error() {
    let mut state = State::new();
    let error = state
        .check_any(0)
        .expect_err("argument index zero is invalid");
    assert_invalid_zero_index(&error);

    let error = state
        .check_type(0, LuaType::Number)
        .expect_err("argument index zero is invalid");
    assert_invalid_zero_index(&error);
}

#[test]
fn stack_editing_rejects_zero_without_panicking_or_mutating_the_stack() {
    let mut state = State::new();
    let errors = [
        state.insert(0).expect_err("index zero is invalid"),
        state.remove(0).expect_err("index zero is invalid"),
        state.replace(0).expect_err("index zero is invalid"),
    ];
    for error in &errors {
        assert_invalid_zero_index(error);
    }
    assert_eq!(state.get_top(), 0);
}

#[test]
fn nan_table_insertion_returns_the_existing_typed_error() {
    let error = support::run_error("local t = {}; t[0 / 0] = 1");
    assert!(matches!(
        error.kind(),
        ErrorKind::TypeError(TypeError::TableKeyNan)
    ));
}

#[test]
fn pattern_errors_keep_their_text_and_expose_structured_details() {
    let error = support::run_error(r#"return string.match("a", ")")"#);
    assert!(matches!(
        error.kind(),
        ErrorKind::PatternError(PatternErrorDetail::InvalidPatternCapture)
    ));
    assert_eq!(error.kind().to_string(), "invalid pattern capture");

    let error = support::run_error(r#"return string.match("aa", "(a)%0")"#);
    assert!(matches!(
        error.kind(),
        ErrorKind::PatternError(PatternErrorDetail::InvalidCaptureIndex(Some(-1)))
    ));
    assert_eq!(error.kind().to_string(), "invalid capture index %0");

    let error = support::run_error(r#"return string.match("a", "[")"#);
    assert!(matches!(
        error.kind(),
        ErrorKind::PatternError(PatternErrorDetail::MalformedPattern(
            MalformedPatternDetail::MissingBracket
        ))
    ));
    assert_eq!(error.kind().to_string(), "malformed pattern (missing ']')");
}

#[test]
fn gmatch_cached_pattern_errors_are_structured_too() {
    let error = support::run_error(r#"local iter = string.gmatch("a", "("); return iter()"#);
    assert!(matches!(
        error.kind(),
        ErrorKind::PatternError(PatternErrorDetail::UnfinishedCapture)
    ));
    assert_eq!(error.kind().to_string(), "unfinished capture");
}
