//! Lexer error variants and source positions, line counting across mixed
//! newline sequences, plus newline call syntax.

use dellingr::State;
use dellingr::error::{Error, ErrorKind, SyntaxError};

use crate::support;

fn load_error(code: &str) -> Error {
    let mut state = State::new();
    dellingr::Program::compile(code, None)
        .and_then(|program| state.load(&program))
        .expect_err(&format!("expected a load error from {code:?}"))
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
fn unclosed_short_string_position_is_independent_of_newline_style() {
    // The error names the end of the line the string broke off on, where its
    // closing quote is missing, whichever newline convention ends that line.
    for source in ["\"a\r\"", "\"a\n\"", "\"a\r\n\"", "\"a\n\r\""] {
        let err = load_error(source);
        assert!(
            matches!(
                err.kind(),
                ErrorKind::SyntaxError(SyntaxError::UnclosedString)
            ),
            "{err}"
        );
        assert_eq!((err.line_num(), err.column()), (1, 3), "{source:?}");
    }
}

#[test]
fn lparen_on_a_new_line_compiles_as_a_call() {
    let mut state = State::new();
    support::load(&mut state, "local f = print\nlocal x = f\n(1)");
}

#[test]
fn crlf_followed_by_cr_is_two_line_breaks() {
    // Reference Lua 5.2 and 5.4 both report line 3 here.
    let err = support::run_error("local x = 1\r\n\rerror('boom')");
    assert_eq!(err.stack_trace()[0].line, 3);
    assert!(format!("{err}").starts_with("3:0: boom"), "{err}");

    let err = support::run_error("local x = 1\n\r\nerror('boom')");
    assert_eq!(err.stack_trace()[0].line, 3);
}
