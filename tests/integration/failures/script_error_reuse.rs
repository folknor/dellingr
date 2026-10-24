//! A script error carries its source and line, and the State stays usable
//! after it.

use dellingr::error::{ErrorKind, ErrorValue};
use dellingr::{ArgCount, RetCount, State};

use crate::support;

#[test]
fn script_error_has_source_line_and_state_remains_reusable() {
    let mut state = State::new();
    dellingr::Program::compile("error('oops')", Some(b"input".as_slice()))
        .and_then(|program| state.load(&program))
        .expect("error source compiles");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("script error must fail");
    assert!(
        matches!(error.kind(), ErrorKind::ScriptError { value: ErrorValue::String(message), .. } if message == b"oops")
    );
    assert_eq!(error.line_num(), 1);
    assert_eq!(
        error.stack_trace()[0].source.as_deref(),
        Some(b"input".as_slice())
    );
    assert!(error.to_string().starts_with("input:1: oops"));

    support::eval(&mut state, "return 7", 1);
    assert_eq!(state.to_number(-1).expect("reuse result is a number"), 7.0);
}
