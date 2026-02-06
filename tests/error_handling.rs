//! Tests for proper error handling (things that used to panic).
//!
//! These tests verify that the VM returns clean errors instead of panicking
//! when it encounters unexpected types or corrupt state.

use lua::error::ErrorKind;
use lua::{ArgCount, RetCount, State};

/// Helper: runs Lua code that returns a number, checks the result.
fn run_number(code: &str) -> f64 {
    let mut state = State::new();
    state.load_string(code).unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .unwrap_or_else(|e| panic!("Error running: {}\n{}", code, e));
    state.to_number(-1).unwrap()
}

/// Helper: runs a Lua string and returns the error, panicking if it succeeds.
fn expect_error(code: &str) -> lua::error::Error {
    let mut state = State::new();
    state.load_string(code).unwrap();
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    result.expect_err(&format!("Expected error from: {}", code))
}

// -- Numeric for-loop tests --

#[test]
fn numeric_for_loop_basic() {
    let sum = run_number(
        r#"
        local sum = 0
        for i = 1, 10 do
            sum = sum + i
        end
        return sum
    "#,
    );
    assert_eq!(sum, 55.0);
}

#[test]
fn numeric_for_loop_step() {
    let sum = run_number(
        r#"
        local sum = 0
        for i = 0, 10, 2 do
            sum = sum + i
        end
        return sum
    "#,
    );
    assert_eq!(sum, 30.0);
}

#[test]
fn numeric_for_loop_negative_step() {
    let sum = run_number(
        r#"
        local sum = 0
        for i = 10, 1, -1 do
            sum = sum + i
        end
        return sum
    "#,
    );
    assert_eq!(sum, 55.0);
}

#[test]
fn numeric_for_loop_empty_range() {
    let count = run_number(
        r#"
        local count = 0
        for i = 10, 1 do
            count = count + 1
        end
        return count
    "#,
    );
    assert_eq!(count, 0.0);
}

// -- Generic for-loop (ipairs) tests --

#[test]
fn ipairs_basic() {
    let sum = run_number(
        r#"
        local t = {10, 20, 30}
        local sum = 0
        for i, v in ipairs(t) do
            sum = sum + v
        end
        return sum
    "#,
    );
    assert_eq!(sum, 60.0);
}

#[test]
fn ipairs_stops_at_nil() {
    let count = run_number(
        r#"
        local t = {10, 20, nil, 40}
        local count = 0
        for i, v in ipairs(t) do
            count = count + 1
        end
        return count
    "#,
    );
    assert_eq!(count, 2.0);
}

// -- Error type tests --

#[test]
fn error_function_produces_error() {
    let err = expect_error("error('user error message')");
    let msg = format!("{}", err);
    assert!(
        msg.contains("user error message"),
        "Error should contain user message, got: {}",
        msg
    );
}

#[test]
fn type_error_on_arithmetic() {
    let err = expect_error("local x = 'hello' + 1");
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError, got: {}",
        err
    );
}

#[test]
fn type_error_on_call() {
    let err = expect_error("local x = 5\nx()");
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError, got: {}",
        err
    );
}

#[test]
fn type_error_on_index() {
    let err = expect_error("local x = 5\nlocal y = x.foo");
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError, got: {}",
        err
    );
}

#[test]
fn type_error_on_table_key_nil() {
    let err = expect_error("local t = {}\nt[nil] = 1");
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError, got: {}",
        err
    );
}

#[test]
fn budget_exceeded_error() {
    let mut state = State::new();
    state.set_cost_budget(10);
    state
        .load_string(
            r#"
        local sum = 0
        for i = 1, 10000 do
            sum = sum + i
        end
    "#,
        )
        .unwrap();
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    let err = result.expect_err("Expected budget error");
    assert!(
        matches!(err.kind, ErrorKind::BudgetExceeded { .. }),
        "Expected BudgetExceeded, got: {}",
        err
    );
}

#[test]
fn call_depth_exceeded_error() {
    let err = expect_error(
        r#"
        local function recurse(n)
            return recurse(n + 1)
        end
        recurse(0)
    "#,
    );
    assert!(
        matches!(err.kind, ErrorKind::CallDepthExceeded { .. }),
        "Expected CallDepthExceeded, got: {}",
        err
    );
}
