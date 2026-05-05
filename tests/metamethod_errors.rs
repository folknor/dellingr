//! Tests for metamethod error handling.
//!
//! Verifies that invalid metamethod handlers produce errors instead of
//! silently returning nil or falling back to raw assignment.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

/// Helper: runs Lua code that returns a number.
fn run_number(code: &str) -> f64 {
    let mut state = State::new();
    state.load_string(code).unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .unwrap_or_else(|e| panic!("Error running: {code}\n{e}"));
    state.to_number(-1).unwrap()
}

/// Helper: runs a Lua string and returns the error.
fn expect_error(code: &str) -> dellingr::error::Error {
    let mut state = State::new();
    state.load_string(code).unwrap();
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    result.expect_err(&format!("Expected error from: {code}"))
}

/// Helper: runs and expects success with no return value.
fn expect_ok(code: &str) {
    let mut state = State::new();
    state.load_string(code).unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .unwrap_or_else(|e| panic!("Error running: {code}\n{e}"));
}

// -- __index: valid usage --

#[test]
fn index_table_handler() {
    let val = run_number(
        r#"
        local defaults = { x = 42 }
        local t = setmetatable({}, { __index = defaults })
        return t.x
    "#,
    );
    assert_eq!(val, 42.0);
}

#[test]
fn index_function_handler() {
    let val = run_number(
        r#"
        local t = setmetatable({}, {
            __index = function(self, key)
                if key == "x" then return 99 end
                return nil
            end
        })
        return t.x
    "#,
    );
    assert_eq!(val, 99.0);
}

#[test]
fn index_existing_key_no_metamethod() {
    // If the key exists, __index should NOT be called
    let val = run_number(
        r#"
        local t = setmetatable({ x = 10 }, {
            __index = function(self, key) return 999 end
        })
        return t.x
    "#,
    );
    assert_eq!(val, 10.0);
}

#[test]
fn field_cache_revalidates_after_table_mutation() {
    let val = run_number(
        r#"
        local t = { a = 1, b = 2, c = 3 }
        local function read_b()
            return t.b
        end

        local first = read_b()
        t.a = nil
        local second = read_b()
        t.b = 7
        local third = read_b()
        return first * 100 + second * 10 + third
    "#,
    );
    assert_eq!(val, 227.0);
}

#[test]
fn field_cache_does_not_cache_index_metamethod_result() {
    let val = run_number(
        r#"
        local calls = 0
        local t = setmetatable({}, {
            __index = function(self, key)
                calls = calls + 1
                return calls
            end
        })
        return t.x + t.x
    "#,
    );
    assert_eq!(val, 3.0);
}

// -- __index: invalid handlers should error --

#[test]
fn index_number_handler_errors() {
    let err = expect_error(
        r#"
        local t = setmetatable({}, { __index = 42 })
        return t.x
    "#,
    );
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError for number __index, got: {err}"
    );
}

#[test]
fn index_string_handler_errors() {
    let err = expect_error(
        r#"
        local t = setmetatable({}, { __index = "not a table" })
        return t.x
    "#,
    );
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError for string __index, got: {err}"
    );
}

#[test]
fn index_boolean_handler_errors() {
    let err = expect_error(
        r#"
        local t = setmetatable({}, { __index = true })
        return t.x
    "#,
    );
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError for boolean __index, got: {err}"
    );
}

// -- __newindex: valid usage --

#[test]
fn newindex_table_handler() {
    let val = run_number(
        r#"
        local storage = {}
        local t = setmetatable({}, { __newindex = storage })
        t.x = 42
        return storage.x
    "#,
    );
    assert_eq!(val, 42.0);
}

#[test]
fn newindex_function_handler() {
    expect_ok(
        r#"
        local log = {}
        local t = setmetatable({}, {
            __newindex = function(self, key, value)
                rawset(self, key, value * 2)
            end
        })
        t.x = 21
    "#,
    );
}

#[test]
fn newindex_existing_key_no_metamethod() {
    // If the key already exists, __newindex should NOT be called
    let val = run_number(
        r#"
        local called = 0
        local t = setmetatable({ x = 10 }, {
            __newindex = function(self, key, value)
                called = called + 1
            end
        })
        t.x = 99  -- existing key, no __newindex
        return called
    "#,
    );
    assert_eq!(val, 0.0, "__newindex should not fire for existing keys");
}

// -- __newindex: invalid handlers should error --

#[test]
fn newindex_number_handler_errors() {
    let err = expect_error(
        r#"
        local t = setmetatable({}, { __newindex = 42 })
        t.x = 1
    "#,
    );
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError for number __newindex, got: {err}"
    );
}

#[test]
fn newindex_string_handler_errors() {
    let err = expect_error(
        r#"
        local t = setmetatable({}, { __newindex = "not a table" })
        t.x = 1
    "#,
    );
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError for string __newindex, got: {err}"
    );
}

#[test]
fn newindex_boolean_handler_errors() {
    let err = expect_error(
        r#"
        local t = setmetatable({}, { __newindex = true })
        t.x = 1
    "#,
    );
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError for boolean __newindex, got: {err}"
    );
}

// -- Metamethod depth limit --

#[test]
fn metamethod_depth_exceeded() {
    let err = expect_error(
        r#"
        local t = {}
        setmetatable(t, { __index = t })
        return t.x
    "#,
    );
    assert!(
        matches!(err.kind, ErrorKind::MetamethodDepthExceeded { .. }),
        "Expected MetamethodDepthExceeded, got: {err}"
    );
}

// -- __index function errors propagate --

#[test]
fn index_function_error_propagates() {
    let err = expect_error(
        r#"
        local t = setmetatable({}, {
            __index = function(self, key)
                error("index error")
            end
        })
        return t.x
    "#,
    );
    let msg = format!("{err}");
    assert!(
        msg.contains("index error"),
        "Error should propagate from __index function, got: {msg}"
    );
}
