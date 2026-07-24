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
    assert!(
        result.is_err(),
        "gsub should propagate replacement function errors"
    );
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
    assert!(
        result.is_err(),
        "gsub should propagate budget exceeded errors"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err.kind, ErrorKind::BudgetExceeded { .. }),
        "Expected BudgetExceeded, got: {err}"
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
    state.call(ArgCount::Fixed(0), RetCount::Fixed(2)).unwrap();
    let result = state.to_string(-2).unwrap();
    assert_eq!(
        result, "hello WORLD",
        "nil return should keep original match"
    );
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
    state.call(ArgCount::Fixed(0), RetCount::Fixed(2)).unwrap();
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
    state.call(ArgCount::Fixed(0), RetCount::Fixed(2)).unwrap();
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
    state.call(ArgCount::Fixed(0), RetCount::Fixed(2)).unwrap();
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
    state.call(ArgCount::Fixed(0), RetCount::Fixed(2)).unwrap();
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
    state.call(ArgCount::Fixed(0), RetCount::Fixed(2)).unwrap();
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
    state.call(ArgCount::Fixed(0), RetCount::Fixed(2)).unwrap();
    let result = state.to_string(-2).unwrap();
    let count = state.to_number(-1).unwrap();
    assert_eq!(result, "hello earth");
    assert_eq!(count, 1.0);
}

#[test]
fn gsub_string_replacement_replaces_all_matches() {
    let mut state = State::new();
    state
        .load_string(
            r#"
        local result, count = string.gsub("hello", "l", "L")
        return result, count
    "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(2)).unwrap();
    let result = state.to_string(-2).unwrap();
    let count = state.to_number(-1).unwrap();
    assert_eq!(result, "heLLo");
    assert_eq!(count, 2.0);
}

#[test]
fn gsub_string_replacement_inside_dynamic_argument() {
    let mut state = State::new();
    state
        .load_string_named(
            r#"
        local function test(name, condition)
            if condition then
                print("[PASS] " .. name)
            else
                print("[FAIL] " .. name)
                error("Test failed: " .. name)
            end
        end

        local a1,a2,a3,a4,a5,a6,a7,a8,a9,a10,a11,a12,a13,a14,a15,a16,a17,a18,a19,a20

        test("pattern captures", (function()
            local a, b = string.match("hello world", "(%a+) (%a+)")
            return a == "hello" and b == "world"
        end)())

        test("pattern gsub count", (function()
            local result, count = string.gsub("hello", "l", "L")
            return result == "heLLo" and count == 2
        end)())
    "#,
            Some("dynamic-gsub.lua".to_string()),
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
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
    state.call(ArgCount::Fixed(0), RetCount::Fixed(2)).unwrap();
    let result = state.to_string(-2).unwrap();
    assert_eq!(result, "20+30=50");
}

fn assert_runtime_error(code: &str, expected: &str) {
    let mut state = State::new();
    state.load_string(code).unwrap();
    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(2))
        .unwrap_err();
    assert!(
        err.to_string().contains(expected),
        "unexpected error: {err}"
    );
}

#[test]
fn malformed_patterns_are_runtime_errors_in_all_wrappers() {
    for code in [
        r#"return string.find("abc", "[")"#,
        r#"return string.match("abc", "[")"#,
        r#"for _ in string.gmatch("abc", "[") do end"#,
        r#"return string.gsub("abc", "[", "x")"#,
    ] {
        assert_runtime_error(code, "malformed pattern");
    }
}

#[test]
fn gsub_validates_replacement_grammar_only_for_actual_replacements() {
    for replacement in ["%q", "%"] {
        assert_runtime_error(
            &format!(r#"return string.gsub("a", "a", "{replacement}")"#),
            "invalid use of '%'",
        );
    }
    assert_runtime_error(
        r#"return string.gsub("a", "(a)", "%2")"#,
        "invalid capture index",
    );

    // A bad replacement is not validated when there is no match or the
    // substitution limit is zero. Bind each gsub's two results to locals so all
    // four values survive (a non-final call in a return list is truncated to
    // one value).
    let mut state = State::new();
    state
        .load_string(
            r#"
            local s1, c1 = string.gsub("a", "b", "%q")
            local s2, c2 = string.gsub("a", "a", "%q", 0)
            return s1, c1, s2, c2
            "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(4)).unwrap();
    assert_eq!(state.to_bytes(-4).unwrap(), b"a");
    assert_eq!(state.to_number(-3).unwrap(), 0.0);
    assert_eq!(state.to_bytes(-2).unwrap(), b"a");
    assert_eq!(state.to_number(-1).unwrap(), 0.0);
}

#[test]
fn gsub_position_captures_and_replacement_escapes() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            local a = string.gsub("ab", "()a", "<%0:%1:%%>")
            local b = string.gsub("ab", "a", "<%1>")
            local c = string.gsub("ab", "()(a)", function(pos, value) return pos .. value end)
            local d = string.gsub("ab", "()(a)", {[1] = "table"})
            return a, b, c, d
            "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(4)).unwrap();
    assert_eq!(state.to_bytes(-4).unwrap(), b"<a:1:%>b");
    assert_eq!(state.to_bytes(-3).unwrap(), b"<a>b");
    assert_eq!(state.to_bytes(-2).unwrap(), b"1ab");
    assert_eq!(state.to_bytes(-1).unwrap(), b"tableb");
}
