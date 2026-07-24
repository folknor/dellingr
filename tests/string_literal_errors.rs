//! Public compilation regressions for malformed and unsupported string syntax.

use dellingr::error::{ErrorKind, SyntaxError};
use dellingr::{Engine, State};

#[test]
fn malformed_escapes_return_syntax_errors_through_state_loading() {
    for (source, expected) in [
        (r#"return "\256""#, "decimal"),
        (r#"return "\999""#, "decimal"),
        (r#"return "\x""#, "hexadecimal"),
        (r#"return "\x4""#, "hexadecimal"),
        (r#"return "\x4G""#, "hexadecimal"),
        (r#"return "\q""#, "invalid"),
    ] {
        let err = State::new()
            .load_string(source)
            .expect_err("malformed escape must not compile");
        assert!(
            matches!(
                (&err.kind, expected),
                (
                    ErrorKind::SyntaxError(SyntaxError::DecimalEscapeTooLarge),
                    "decimal"
                ) | (
                    ErrorKind::SyntaxError(SyntaxError::HexadecimalDigitExpected),
                    "hexadecimal"
                ) | (
                    ErrorKind::SyntaxError(SyntaxError::InvalidEscapeSequence),
                    "invalid"
                )
            ),
            "{err}"
        );
    }
}

#[test]
fn long_strings_return_syntax_errors_through_public_compilation() {
    for source in ["[[hello]]", "[=[x]=]", "[["] {
        let err = Engine::new()
            .compile(source)
            .expect_err("long strings must not compile");
        assert!(matches!(
            err.kind,
            ErrorKind::SyntaxError(SyntaxError::LongStringUnsupported)
        ));
    }
}
