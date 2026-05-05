//! Tests for proper error handling (things that used to panic).
//!
//! These tests verify that the VM returns clean errors instead of panicking
//! when it encounters unexpected types or corrupt state.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

/// Helper: runs Lua code that returns a number, checks the result.
fn run_number(code: &str) -> f64 {
    let mut state = State::new();
    state.load_string(code).unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .unwrap_or_else(|e| panic!("Error running: {code}\n{e}"));
    state.to_number(-1).unwrap()
}

/// Helper: runs a Lua string and returns the error, panicking if it succeeds.
fn expect_error(code: &str) -> dellingr::error::Error {
    let mut state = State::new();
    state.load_string(code).unwrap();
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    result.expect_err(&format!("Expected error from: {code}"))
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

#[test]
fn ipairs_uses_index_metamethod_for_holes() {
    let sum = run_number(
        r#"
        local t = setmetatable({10}, {
            __index = function(self, key)
                if key == 2 then return 20 end
                return nil
            end
        })
        local sum = 0
        for i, v in ipairs(t) do
            sum = sum + v
        end
        return sum
    "#,
    );
    assert_eq!(sum, 30.0);
}

// -- Error type tests --

#[test]
fn error_function_produces_error() {
    let err = expect_error("error('user error message')");
    let msg = format!("{err}");
    assert!(
        msg.contains("user error message"),
        "Error should contain user message, got: {msg}"
    );
}

#[test]
fn type_error_on_arithmetic() {
    let err = expect_error("local x = 'hello' + 1");
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError, got: {err}"
    );
}

#[test]
fn type_error_on_call() {
    let err = expect_error("local x = 5\nx()");
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError, got: {err}"
    );
}

#[test]
fn type_error_on_index() {
    let err = expect_error("local x = 5\nlocal y = x.foo");
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError, got: {err}"
    );
}

#[test]
fn type_error_on_table_key_nil() {
    let err = expect_error("local t = {}\nt[nil] = 1");
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError, got: {err}"
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
        "Expected BudgetExceeded, got: {err}"
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
        "Expected CallDepthExceeded, got: {err}"
    );
}

// -- Table operation tests --

#[test]
fn table_insert_append() {
    let val = run_number(
        r#"
        local t = {1, 2, 3}
        table.insert(t, 4)
        return #t
    "#,
    );
    assert_eq!(val, 4.0);
}

#[test]
fn table_insert_at_position() {
    let val = run_number(
        r#"
        local t = {1, 2, 3}
        table.insert(t, 2, 99)
        return t[2]
    "#,
    );
    assert_eq!(val, 99.0);
}

#[test]
fn table_remove_basic() {
    let val = run_number(
        r#"
        local t = {10, 20, 30}
        local removed = table.remove(t, 2)
        return removed
    "#,
    );
    assert_eq!(val, 20.0);
}

#[test]
fn table_nil_assignment_deletes_key() {
    let val = run_number(
        r#"
        local t = {a = 1, b = 2}
        t.a = nil

        if rawget(t, "a") ~= nil then
            return -1
        end

        local count = 0
        local saw_a = 0
        for k, v in pairs(t) do
            count = count + 1
            if k == "a" then
                saw_a = saw_a + 1
            end
        end

        return count * 10 + saw_a
    "#,
    );
    assert_eq!(val, 10.0);
}

#[test]
fn table_sort_basic() {
    let val = run_number(
        r#"
        local t = {3, 1, 2}
        table.sort(t)
        return t[1] * 100 + t[2] * 10 + t[3]
    "#,
    );
    assert_eq!(val, 123.0);
}

#[test]
fn table_sort_with_comparator() {
    let val = run_number(
        r#"
        local t = {1, 2, 3}
        table.sort(t, function(a, b) return a > b end)
        return t[1] * 100 + t[2] * 10 + t[3]
    "#,
    );
    assert_eq!(val, 321.0);
}

#[test]
fn table_concat_basic() {
    let mut state = State::new();
    state
        .load_string(r#"return table.concat({1, 2, 3}, ", ")"#)
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_string(-1).unwrap(), "1, 2, 3");
}

#[test]
fn table_unpack_basic() {
    let val = run_number(
        r#"
        local a, b, c = table.unpack({10, 20, 30})
        return a + b + c
    "#,
    );
    assert_eq!(val, 60.0);
}

// -- Error message quality tests --

/// Helper: tries to load+call Lua code and returns the error, panicking if it succeeds.
/// Uses RetCount::Void since we only care about the error.
fn expect_load_or_run_error(code: &str) -> dellingr::error::Error {
    let mut state = State::new();
    if let Err(e) = state.load_string(code) {
        return e;
    }
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    result.expect_err(&format!("Expected error from: {code}"))
}

#[test]
fn error_msg_unexpected_token_includes_context() {
    // Using 'end' where an expression is expected
    let err = expect_load_or_run_error("local x = end");
    let msg = format!("{err}");
    assert!(
        msg.contains("expected") || msg.contains("near"),
        "Error should describe what was expected, got: {msg}"
    );
}

#[test]
fn error_msg_vararg_outside_vararg_function() {
    // '...' inside a non-vararg function should error
    let err = expect_load_or_run_error("local function f() return ... end");
    let msg = format!("{err}");
    assert!(
        msg.contains("vararg"),
        "Error should mention vararg, got: {msg}"
    );
}

#[test]
fn error_msg_vararg_in_table_outside_vararg_function() {
    let err = expect_load_or_run_error("local function f() return {...} end");
    let msg = format!("{err}");
    assert!(
        msg.contains("vararg"),
        "Error should mention vararg, got: {msg}"
    );
}

#[test]
fn error_msg_missing_end_keyword() {
    let err = expect_load_or_run_error("if true then local x = 1");
    let msg = format!("{err}");
    // Should get unexpected EOF (missing 'end')
    assert!(
        msg.contains("<eof>") || msg.contains("expected"),
        "Error should mention <eof> or expected, got: {msg}"
    );
}

#[test]
fn error_msg_type_error_arithmetic() {
    let err = expect_error("local x = 'hello' + 1");
    let msg = format!("{err}");
    assert!(
        msg.contains("arithmetic") && msg.contains("string"),
        "Arithmetic type error should mention 'arithmetic' and 'string', got: {msg}"
    );
}

#[test]
fn error_msg_type_error_call() {
    let err = expect_error("local x = 5\nx()");
    let msg = format!("{err}");
    assert!(
        msg.contains("call") && msg.contains("number"),
        "Call type error should mention 'call' and 'number', got: {msg}"
    );
}

#[test]
fn error_msg_type_error_index() {
    let err = expect_error("local x = 5\nlocal y = x.foo");
    let msg = format!("{err}");
    assert!(
        msg.contains("index") && msg.contains("number"),
        "Index type error should mention 'index' and 'number', got: {msg}"
    );
}

#[test]
fn error_msg_budget_exceeded_shows_amounts() {
    let mut state = State::new();
    state.set_cost_budget(10);
    state
        .load_string("local s = 0\nfor i = 1, 10000 do s = s + i end")
        .unwrap();
    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("Expected budget error");
    let msg = format!("{err}");
    assert!(
        msg.contains("budget") && msg.contains("10"),
        "Budget error should show budget amount, got: {msg}"
    );
}

#[test]
fn global_lookup_cache_respects_restricted_env() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            x = 1
            function read_x()
                if x == nil then return 2 end
                return x
            end
        "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    state.get_global("read_x");
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 1.0);
    state.pop(1);

    let restricted = state.with_restricted_env(&["read_x"], |state| {
        state.get_global("read_x");
        state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
        let result = state.to_number(-1).unwrap();
        state.pop(1);
        result
    });
    assert_eq!(restricted, 2.0);

    state.get_global("read_x");
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 1.0);
}

#[test]
fn error_msg_call_depth_shows_overflow() {
    let err = expect_error("local function r(n) return r(n+1) end\nr(0)");
    let msg = format!("{err}");
    assert!(
        msg.contains("call stack overflow") || msg.contains("depth"),
        "Call depth error should mention overflow/depth, got: {msg}"
    );
}
