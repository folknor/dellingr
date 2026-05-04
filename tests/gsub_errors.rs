//! Tests for string.gsub error propagation and nil/false handling.
//!
//! Verifies that:
//! - Errors in replacement functions are propagated (not swallowed)
//! - Errors in table lookups via metamethods are propagated
//! - nil/false results from functions and tables keep the original match

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

// -- Error propagation tests --

#[test]
fn gsub_function_error_is_propagated() {
    let mut state = State::new();
    state
        .load_string(
            r#"
        return string.gsub("hello world", "%w+", function(m)
            error("boom")
        end)
    "#,
        )
        .unwrap();
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(2));
    assert!(result.is_err(), "gsub should propagate replacement function errors");
}

#[test]
fn gsub_function_budget_error_is_propagated() {
    let mut state = State::new();
    state.set_cost_budget(50);
    state
        .load_string(
            r#"
        -- replacement function that burns budget with a tight loop
        return string.gsub("aaa", "%w", function(m)
            local x = 0
            for i = 1, 10000 do x = x + i end
            return m
        end)
    "#,
        )
        .unwrap();
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(2));
    assert!(result.is_err(), "gsub should propagate budget exceeded errors");
    let err = result.unwrap_err();
    assert!(
        matches!(err.kind, ErrorKind::BudgetExceeded { .. }),
        "Expected BudgetExceeded, got: {}",
        err
    );
}

#[test]
fn gsub_table_metamethod_error_is_propagated() {
    let mut state = State::new();
    state
        .load_string(
            r#"
        local t = setmetatable({}, {
            __index = function(self, key)
                error("metamethod error")
            end
        })
        return string.gsub("hello", "%w+", t)
    "#,
        )
        .unwrap();
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(2));
    assert!(
        result.is_err(),
        "gsub should propagate table metamethod errors"
    );
}

// -- nil/false handling tests --

#[test]
fn gsub_function_nil_keeps_original() {
    let mut state = State::new();
    state
        .load_string(
            r#"
        return string.gsub("hello world", "%w+", function(m)
            if m == "hello" then return nil end
            return string.upper(m)
        end)
    "#,
        )
        .unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(2))
        .unwrap();
    let result = state.to_string(-2).unwrap();
    assert_eq!(result, "hello WORLD", "nil return should keep original match");
}

#[test]
fn gsub_function_false_keeps_original() {
    let mut state = State::new();
    state
        .load_string(
            r#"
        return string.gsub("hello world", "%w+", function(m)
            if m == "hello" then return false end
            return string.upper(m)
        end)
    "#,
        )
        .unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(2))
        .unwrap();
    let result = state.to_string(-2).unwrap();
    assert_eq!(
        result, "hello WORLD",
        "false return should keep original match"
    );
}

#[test]
fn gsub_table_nil_keeps_original() {
    let mut state = State::new();
    state
        .load_string(
            r#"
        local t = { world = "WORLD" }
        -- "hello" is not in the table, so it should stay as-is
        return string.gsub("hello world", "%w+", t)
    "#,
        )
        .unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(2))
        .unwrap();
    let result = state.to_string(-2).unwrap();
    assert_eq!(
        result, "hello WORLD",
        "missing table key should keep original match"
    );
}

#[test]
fn gsub_table_false_keeps_original() {
    let mut state = State::new();
    state
        .load_string(
            r#"
        local t = { hello = false, world = "WORLD" }
        return string.gsub("hello world", "%w+", t)
    "#,
        )
        .unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(2))
        .unwrap();
    let result = state.to_string(-2).unwrap();
    assert_eq!(
        result, "hello WORLD",
        "false table value should keep original match"
    );
}

// -- Basic correctness tests --

#[test]
fn gsub_function_replacement_works() {
    let mut state = State::new();
    state
        .load_string(
            r#"
        return string.gsub("hello world", "%w+", function(m)
            return string.upper(m)
        end)
    "#,
        )
        .unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(2))
        .unwrap();
    let result = state.to_string(-2).unwrap();
    let count = state.to_number(-1).unwrap();
    assert_eq!(result, "HELLO WORLD");
    assert_eq!(count, 2.0);
}

#[test]
fn gsub_table_replacement_works() {
    let mut state = State::new();
    state
        .load_string(
            r#"
        local t = { hello = "HI", world = "EARTH" }
        return string.gsub("hello world", "%w+", t)
    "#,
        )
        .unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(2))
        .unwrap();
    let result = state.to_string(-2).unwrap();
    let count = state.to_number(-1).unwrap();
    assert_eq!(result, "HI EARTH");
    assert_eq!(count, 2.0);
}

#[test]
fn gsub_string_replacement_works() {
    let mut state = State::new();
    state
        .load_string(r#"return string.gsub("hello world", "world", "earth")"#)
        .unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(2))
        .unwrap();
    let result = state.to_string(-2).unwrap();
    let count = state.to_number(-1).unwrap();
    assert_eq!(result, "hello earth");
    assert_eq!(count, 1.0);
}

#[test]
fn gsub_with_captures_function() {
    let mut state = State::new();
    state
        .load_string(
            r#"
        return string.gsub("2+3=5", "(%d+)", function(n)
            return tostring(tonumber(n) * 10)
        end)
    "#,
        )
        .unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(2))
        .unwrap();
    let result = state.to_string(-2).unwrap();
    assert_eq!(result, "20+30=50");
}
