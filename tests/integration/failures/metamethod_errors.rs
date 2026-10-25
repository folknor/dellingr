//! Tests for metamethod error handling.
//!
//! Verifies that invalid metamethod handlers produce errors instead of
//! silently returning nil or falling back to raw assignment.

use dellingr::error::{ErrorKind, TypeError};
use dellingr::{LuaType, State};

use crate::support;

/// Helper: runs Lua code that returns a string.
fn run_string(code: &str) -> String {
    let mut state = State::new();
    support::eval(&mut state, code, 1);
    state.to_string(-1).expect("script returns a string")
}

// -- __index: valid usage --

#[test]
fn index_table_handler() {
    let val = support::eval_number(
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
    let val = support::eval_number(
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
    let val = support::eval_number(
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
fn protected_metatables_hide_and_prevent_replacement() {
    let value = support::eval_number(
        r#"
        local t = setmetatable({}, { __metatable = false })
        if getmetatable(t) ~= false then error("a protected metatable did not read as false") end
        return 1
    "#,
    );
    assert_eq!(value, 1.0);
    let err = support::run_error(
        "local t = setmetatable({}, { __metatable = 'locked' }); setmetatable(t, {})",
    );
    assert!(matches!(
        err.kind(),
        ErrorKind::LibraryError(dellingr::error::LibraryError::ProtectedMetatable)
    ));
}

#[test]
fn setmetatable_ignores_extra_arguments() {
    // A trailing argument must be ignored (Lua does), so the replacement
    // metatable is the one installed - not the extra value.
    let value = support::eval_number(
        r#"
        local mt = {}
        local t = setmetatable({}, mt, 42)
        if getmetatable(t) ~= mt then error("the extra argument replaced the metatable") end
        return 1
    "#,
    );
    assert_eq!(value, 1.0);
}

#[test]
fn unprotected_metatables_remain_visible_and_replaceable() {
    let value = support::eval_number(
        r#"
        local mt = { __metatable = nil }
        local t = setmetatable({}, mt)
        if getmetatable(t) ~= mt then error("the unprotected metatable is not visible") end
        local replacement = {}
        setmetatable(t, replacement)
        if getmetatable(t) ~= replacement then error("the unprotected metatable was not replaced") end
        return 1
    "#,
    );
    assert_eq!(value, 1.0);
}

// -- __index: invalid handlers should error --

#[test]
fn index_number_handler_errors() {
    let err = support::run_error(
        r#"
        local t = setmetatable({}, { __index = 42 })
        return t.x
    "#,
    );
    assert!(
        matches!(err.kind(), ErrorKind::TypeError(_)),
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
    type KindCheck = fn(&ErrorKind) -> bool;
    let cases: [(&str, KindCheck); 5] = [
        ("local f = function() end; return f + 1", |kind| {
            matches!(
                kind,
                ErrorKind::TypeError(TypeError::Arithmetic(LuaType::Function))
            )
        }),
        ("local f = function() end; return f .. 'x'", |kind| {
            matches!(
                kind,
                ErrorKind::TypeError(TypeError::Concat(LuaType::Function))
            )
        }),
        ("local f = function() end; return #f", |kind| {
            matches!(
                kind,
                ErrorKind::TypeError(TypeError::Length(LuaType::Function))
            )
        }),
        ("local f = function() end; return f < 1", |kind| {
            matches!(
                kind,
                ErrorKind::TypeError(TypeError::Comparison(LuaType::Function, LuaType::Number))
            )
        }),
        ("local f = function() end; return f.x", |kind| {
            matches!(
                kind,
                ErrorKind::TypeError(TypeError::TableIndex(LuaType::Function))
            )
        }),
    ];
    for (code, has_expected_kind) in cases {
        let error = support::run_error(code);
        assert!(
            has_expected_kind(error.kind()),
            "unexpected error kind for {code}: {error}"
        );
    }
}

#[test]
fn index_boolean_handler_errors() {
    let err = support::run_error(
        r#"
        local t = setmetatable({}, { __index = true })
        return t.x
    "#,
    );
    assert!(
        matches!(err.kind(), ErrorKind::TypeError(_)),
        "Expected TypeError for boolean __index, got: {err}"
    );
}

// -- __newindex: valid usage --

#[test]
fn newindex_table_handler() {
    let val = support::eval_number(
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
    let val = support::eval_number(
        r#"
        local t = setmetatable({}, {
            __newindex = function(self, key, value)
                rawset(self, key, value * 2)
            end
        })
        t.x = 21
        return t.x
    "#,
    );
    assert_eq!(val, 42.0);
}

#[test]
fn newindex_existing_key_no_metamethod() {
    // If the key already exists, __newindex should NOT be called
    let val = support::eval_number(
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
    let err = support::run_error(
        r#"
        local t = setmetatable({}, { __newindex = 42 })
        t.x = 1
    "#,
    );
    assert!(
        matches!(err.kind(), ErrorKind::TypeError(_)),
        "Expected TypeError for number __newindex, got: {err}"
    );
}

#[test]
fn newindex_string_handler_errors() {
    let err = support::run_error(
        r#"
        local t = setmetatable({}, { __newindex = "not a table" })
        t.x = 1
    "#,
    );
    assert!(
        matches!(err.kind(), ErrorKind::TypeError(_)),
        "Expected TypeError for string __newindex, got: {err}"
    );
}

#[test]
fn newindex_boolean_handler_errors() {
    let err = support::run_error(
        r#"
        local t = setmetatable({}, { __newindex = true })
        t.x = 1
    "#,
    );
    assert!(
        matches!(err.kind(), ErrorKind::TypeError(_)),
        "Expected TypeError for boolean __newindex, got: {err}"
    );
}

// -- Metamethod depth limit --

#[test]
fn metamethod_depth_exceeded() {
    let err = support::run_error(
        r#"
        local t = {}
        setmetatable(t, { __index = t })
        return t.x
    "#,
    );
    assert!(
        matches!(err.kind(), ErrorKind::MetamethodDepthExceeded { .. }),
        "Expected MetamethodDepthExceeded, got: {err}"
    );
}

// -- __index function errors propagate --

#[test]
fn index_function_error_propagates() {
    let err = support::run_error(
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
