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

/// Helper: runs Lua code that returns a string.
fn run_string(code: &str) -> String {
    let mut state = State::new();
    state.load_string(code).expect("source compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .unwrap_or_else(|e| panic!("Error running: {code}\n{e}"));
    state.to_string(-1).expect("script returns a string")
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
fn cached_fields_do_not_resurrect_after_deletion() {
    // Each field callsite gets its own cache slot, so the access has to happen
    // inside a function called both before and after the deletion. Two separate
    // `t.x` expressions would never share a warm cache and would pass even if
    // the tombstone were resurrected.
    // Encodes both reads into one number so the warming read is checked too:
    // 1 before the deletion, 9 (via __index) after.
    assert_eq!(
        run_number(
            r#"
            local t = setmetatable({ x = 1 }, { __index = function() return 9 end })
            local function read() return t.x end
            local warm = read()
            t.x = nil
            return warm * 100 + read()
        "#
        ),
        109.0
    );
    // Same for the write cache: the warming assignment must be non-nil and go
    // through the same callsite, so the IC records (key -> index) against a live
    // slot. Removal does not bump the table version, so a `set_at_index` that
    // failed to refuse dead slots would write straight through the tombstone and
    // never reach __newindex.
    // The warming write must land on the live field directly (leaving seen at 0
    // and t.x at 5); only the post-deletion write should reach __newindex. The
    // encoding keeps the intermediate state observable: 0 * 1000 + 5 * 100 + 7.
    assert_eq!(
        run_number(
            r#"
            local seen = 0
            local t = setmetatable({ x = 1 }, { __newindex = function(_, _, v) seen = v end })
            local function write(v) t.x = v end
            write(5)
            local warmed_seen = seen
            local warmed_field = t.x
            t.x = nil
            write(7)
            return warmed_seen * 1000 + warmed_field * 100 + seen
        "#
        ),
        507.0
    );
}

#[test]
fn protected_metatables_hide_and_prevent_replacement() {
    let value = run_number(
        r#"
        local t = setmetatable({}, { __metatable = false })
        return getmetatable(t) == false and 1 or 0
    "#,
    );
    assert_eq!(value, 1.0);
    let err =
        expect_error("local t = setmetatable({}, { __metatable = 'locked' }); setmetatable(t, {})");
    assert!(
        matches!(&err.kind, ErrorKind::RuntimeError(message) if message == "cannot change a protected metatable")
    );
}

#[test]
fn setmetatable_ignores_extra_arguments() {
    // A trailing argument must be ignored (Lua does), so the replacement
    // metatable is the one installed - not the extra value.
    let value = run_number(
        r#"
        local mt = {}
        local t = setmetatable({}, mt, 42)
        return getmetatable(t) == mt and 1 or 0
    "#,
    );
    assert_eq!(value, 1.0);
}

#[test]
fn unprotected_metatables_remain_visible_and_replaceable() {
    let value = run_number(
        r#"
        local mt = { __metatable = nil }
        local t = setmetatable({}, mt)
        local visible = getmetatable(t) == mt
        local replacement = {}
        setmetatable(t, replacement)
        return visible and getmetatable(t) == replacement and 1 or 0
    "#,
    );
    assert_eq!(value, 1.0);
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

#[test]
fn set_field_cache_value_update_takes_fast_path() {
    let val = run_number(
        r#"
        local t = { count = 0 }
        for i = 1, 100 do
            t.count = t.count + 1
        end
        return t.count
    "#,
    );
    assert_eq!(val, 100.0);
}

#[test]
fn set_field_cache_revalidates_after_remove() {
    let val = run_number(
        r#"
        local t = { a = 1, b = 2, c = 3 }
        local function set_b(v)
            t.b = v
        end

        set_b(20)
        local first = t.b
        t.a = nil
        set_b(200)
        local second = t.b
        return first * 1000 + second
    "#,
    );
    assert_eq!(val, 20200.0);
}

#[test]
fn set_field_cache_does_not_skip_newindex_for_new_key() {
    let val = run_number(
        r#"
        local backing = {}
        local hits = 0
        local t = setmetatable({}, {
            __newindex = function(_, k, v)
                hits = hits + 1
                rawset(backing, k, v)
            end
        })
        for i = 1, 5 do
            t.x = i
        end
        return hits * 100 + backing.x
    "#,
    );
    assert_eq!(val, 505.0);
}

#[test]
fn set_field_cache_handles_assigning_nil_as_delete() {
    let val = run_number(
        r#"
        local t = { x = 1, y = 2, z = 3 }
        t.y = nil
        local count = 0
        for _ in pairs(t) do
            count = count + 1
        end
        return count
    "#,
    );
    assert_eq!(val, 2.0);
}

#[test]
fn method_cache_revalidates_index_table_method_update() {
    let val = run_number(
        r#"
        local methods = {}
        function methods:f()
            return 1
        end

        local t = setmetatable({}, { __index = methods })
        local first = t:f()

        function methods:f()
            return 2
        end

        local second = t:f()
        return first * 10 + second
    "#,
    );
    assert_eq!(val, 12.0);
}

#[test]
fn method_cache_revalidates_index_table_reassignment() {
    let val = run_number(
        r#"
        local methods_a = {}
        local methods_b = {}
        function methods_a:f()
            return 1
        end
        function methods_b:f()
            return 2
        end

        local mt = { __index = methods_a }
        local t = setmetatable({}, mt)
        local first = t:f()
        mt.__index = methods_b
        local second = t:f()
        return first * 10 + second
    "#,
    );
    assert_eq!(val, 12.0);
}

#[test]
fn method_cache_does_not_cache_index_function_result() {
    let val = run_number(
        r#"
        local calls = 0
        local t = setmetatable({}, {
            __index = function(self, key)
                calls = calls + 1
                return function()
                    return calls
                end
            end
        })
        return t:f() + t:f()
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
fn index_string_handler_uses_string_library_for_dot_and_bracket_keys() {
    let value = run_string(
        r#"
        local t = setmetatable({}, { __index = "not a table" })
        return type(t.upper) .. "," .. type(t["upper"]) .. "," .. type(t[1])
    "#,
    );
    assert_eq!(value, "function,function,nil");
}

#[test]
fn function_type_errors_name_functions() {
    for code in [
        "local f = function() end; return f + 1",
        "local f = function() end; return f .. 'x'",
        "local f = function() end; return #f",
        "local f = function() end; return f < 1",
        "local f = function() end; return f.x",
    ] {
        let error = expect_error(code);
        assert!(
            error.to_string().contains("function"),
            "function error must name function: {error}"
        );
    }
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
