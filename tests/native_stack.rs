//! Recursion limits that bound native-stack growth, driven to their
//! boundaries on a thread with the stack docs/limits.md states they fit in.

use dellingr::error::ErrorKind;
use dellingr::limits::{MAX_METAMETHOD_DEPTH, MAX_NATIVE_CALL_DEPTH};
use dellingr::{ArgCount, RetCount, State};

#[path = "common/stack.rs"]
mod stack;

fn run(source: &str) -> dellingr::Result<()> {
    let mut state = State::new();
    state.load_string(source)?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
}

#[test]
fn recursive_index_function_stops_at_the_native_call_limit() {
    // The host call is the first call made from Rust and every handler call
    // another, so the 200th handler is the one refused: one level before the
    // metamethod depth would refuse it.
    assert_eq!(MAX_NATIVE_CALL_DEPTH, MAX_METAMETHOD_DEPTH);
    stack::on_limit_test_stack("recursive_index_function", || {
        let error = run(r#"
            local t = setmetatable({}, {__index = function(t, k) return t[k + 1] end})
            local _ = t[1]
            "#)
        .expect_err("unbounded __index recursion must fail");
        match error.kind() {
            ErrorKind::NativeCallDepthExceeded { depth, limit } => {
                assert_eq!(*depth, MAX_NATIVE_CALL_DEPTH);
                assert_eq!(*limit, MAX_NATIVE_CALL_DEPTH);
            }
            other => panic!("expected NativeCallDepthExceeded, got {other}"),
        }
    });
}
