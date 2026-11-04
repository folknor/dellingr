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
fn incomplete_numbers_at_eof_are_recoverable() {
    for source in [
        "x = 1e",
        "x = 1e+",
        "x = 0x",
        "x = 0x.",
        "x = 0x1p",
        "x = 0x1p-",
    ] {
        let err = load_error(source);
        assert!(
            matches!(
                err.kind(),
                ErrorKind::SyntaxError(SyntaxError::UnfinishedNumber)
            ),
            "{source:?}: {err}"
        );
        assert!(err.is_recoverable(), "{source:?}");
        assert!(err.kind().is_recoverable(), "{source:?}");
        assert!(format!("{err}").contains("malformed number"), "{err}");
    }

    for source in ["x = 1e+a", "x = 1e ", "x = 0xg"] {
        let err = load_error(source);
        assert!(matches!(
            err.kind(),
            ErrorKind::SyntaxError(SyntaxError::BadNumber)
        ));
        assert!(!err.is_recoverable(), "{source:?}");
    }
}

#[test]
fn malformed_numerals_are_lexer_errors_wherever_they_appear() {
    // Each case is (source, unfinished, column). The numeral is diagnosed when
    // it is lexed, at its first byte, even where the grammar wanted another
    // token. Recoverability is a claim about the last token alone: `local 1e`
    // ends inside an incomplete numeral although no completion parses, as
    // `local "abc` ends inside an unfinished string.
    for (source, unfinished, column) in [
        ("local 1e", true, 7),
        ("local 1e = 2", false, 7),
        ("t = { a 1e }", false, 9),
        ("t = { a 1e", true, 9),
        ("print(1...2)", false, 7),
        ("print(0x1...2)", false, 7),
        ("print(1.5..'a')", false, 7),
        ("x = 1e5..2", false, 5),
        ("x = 1..2", false, 5),
    ] {
        let err = load_error(source);
        let is_unfinished = match err.kind() {
            ErrorKind::SyntaxError(SyntaxError::UnfinishedNumber) => true,
            ErrorKind::SyntaxError(SyntaxError::BadNumber) => false,
            _ => panic!("{source:?}: expected a numeral error, got {err}"),
        };
        assert_eq!(is_unfinished, unfinished, "{source:?}");
        assert_eq!(err.is_recoverable(), unfinished, "{source:?}");
        assert_eq!((err.line_num(), err.column()), (1, column), "{source:?}");
        assert!(format!("{err}").contains("malformed number"), "{err}");
    }
}

#[test]
fn appending_digits_completes_incomplete_numerals() {
    for (prefix, digits) in [
        ("return 1e", "5"),
        ("return 0x", "1"),
        ("return 0x.", "8"),
        ("return 0x1p", "0"),
    ] {
        let source = format!("{prefix}{digits}");
        dellingr::Program::compile(&source, None)
            .expect("appended digits must complete the numeral");
    }
}

#[test]
fn short_string_ending_at_eof_is_recoverable() {
    for source in ["print(\"abc", "print(\"abc\\"] {
        let err = load_error(source);
        assert!(
            matches!(
                err.kind(),
                ErrorKind::SyntaxError(SyntaxError::UnfinishedString)
            ),
            "{source:?}: {err}"
        );
        assert!(err.is_recoverable(), "{source:?}");
        assert!(err.kind().is_recoverable(), "{source:?}");
        assert!(format!("{err}").contains("unfinished string"), "{err}");
    }
}

#[test]
fn goto_is_reserved_and_reports_that_the_feature_is_unsupported() {
    for source in [
        "local goto = 1",
        "goto = 5",
        "goto",
        "goto continue",
        "local value = goto",
        "value.goto",
    ] {
        let err = load_error(source);
        assert!(
            matches!(
                err.kind(),
                ErrorKind::SyntaxError(SyntaxError::UnsupportedGoto)
            ),
            "{source:?}: {err}"
        );
        assert!(
            format!("{err}").contains("the 'goto' keyword is not supported"),
            "{err}"
        );
    }
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
        assert!(!err.is_recoverable(), "{source:?}");
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
    assert!(format!("{err}").starts_with("3: boom"), "{err}");

    let err = support::run_error("local x = 1\n\r\nerror('boom')");
    assert_eq!(err.stack_trace()[0].line, 3);
}
