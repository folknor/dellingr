//! Source-location and message diagnostics: line counting across mixed
//! newline sequences, lexer error variants, and the `Error` prefix's
//! line/column pairing.

use dellingr::error::{Error, ErrorKind, PrefixLocation, StackFrame, SyntaxError};
use dellingr::{ArgCount, RetCount, State};

fn load_error(code: &str) -> Error {
    let mut state = State::new();
    dellingr::Program::compile(code, None)
        .and_then(|program| state.load(&program))
        .expect_err(&format!("expected a load error from {code:?}"))
}

fn run_error(code: &str) -> Error {
    let mut state = State::new();
    dellingr::Program::compile(code, None)
        .and_then(|program| state.load(&program))
        .expect("program must compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("program must fail")
}

fn frame(line: u32) -> StackFrame {
    StackFrame {
        function_name: None,
        source: None,
        line,
    }
}

#[test]
fn crlf_followed_by_cr_is_two_line_breaks() {
    // Reference Lua 5.2 and 5.4 both report line 3 here.
    let err = run_error("local x = 1\r\n\rerror('boom')");
    assert_eq!(err.stack_trace()[0].line, 3);
    assert!(format!("{err}").starts_with("3:0: boom"), "{err}");

    let err = run_error("local x = 1\n\r\nerror('boom')");
    assert_eq!(err.stack_trace()[0].line, 3);
}

#[test]
fn unfinished_long_comment_has_its_own_message() {
    let err = load_error("x = 1\n--[[ never closed\nx = 2");
    assert!(matches!(
        err.kind(),
        ErrorKind::SyntaxError(SyntaxError::UnfinishedLongComment)
    ));
    assert_eq!((err.line_num(), err.column()), (2, 1));
    assert!(err.is_recoverable());
    assert!(
        format!("{err}").contains("unfinished long comment"),
        "{err}"
    );
}

#[test]
fn lparen_on_a_new_line_compiles_as_a_call() {
    let mut state = State::new();
    dellingr::Program::compile("local f = print\nlocal x = f\n(1)", None)
        .and_then(|program| state.load(&program))
        .expect("a `(` on a new line continues the call, as in reference Lua");
}

#[test]
fn selected_caller_frame_does_not_borrow_the_innermost_column() {
    let kind = |prefix| ErrorKind::ScriptError {
        value: dellingr::error::ErrorValue::String(b"boom".to_vec()),
        prefix,
    };

    // `error(msg, 2)`: the caller's line must not be paired with the
    // callee's column.
    let caller = Error::new(kind(PrefixLocation::TraceFrame(1)), 1, 7)
        .with_stack_trace(vec![frame(1), frame(2)]);
    assert!(format!("{caller}").starts_with("2:0: boom"), "{caller}");

    // The innermost frame is the error site, so its column still applies.
    let current =
        Error::new(kind(PrefixLocation::Current), 1, 7).with_stack_trace(vec![frame(1), frame(2)]);
    assert!(format!("{current}").starts_with("1:7: boom"), "{current}");

    let explicit_current =
        Error::new(kind(PrefixLocation::TraceFrame(0)), 1, 7).with_stack_trace(vec![frame(1)]);
    assert!(
        format!("{explicit_current}").starts_with("1:7: boom"),
        "{explicit_current}"
    );

    // Matching line numbers alone do not make a caller the error's own frame.
    let same_line = Error::new(kind(PrefixLocation::TraceFrame(1)), 1, 7)
        .with_stack_trace(vec![frame(1), frame(1)]);
    assert!(
        format!("{same_line}").starts_with("1:0: boom"),
        "{same_line}"
    );

    let different_line =
        Error::new(kind(PrefixLocation::Current), 1, 7).with_stack_trace(vec![frame(2)]);
    assert!(
        format!("{different_line}").starts_with("2:0: boom"),
        "{different_line}"
    );

    // An unknown selected caller line must not fall back to the callee's site.
    let unknown = Error::new(kind(PrefixLocation::TraceFrame(1)), 1, 7)
        .with_stack_trace(vec![frame(1), frame(0)]);
    assert!(format!("{unknown}").starts_with("0:0: boom"), "{unknown}");
}
