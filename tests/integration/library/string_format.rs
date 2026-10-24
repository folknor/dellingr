use dellingr::error::{ErrorKind, ErrorValue};
use dellingr::{ArgCount, RetCount, State};

use crate::support;

fn expect_runtime_error(code: &str, expected: &str) {
    let mut state = State::new();
    support::load(&mut state, code);
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("format call must fail");
    assert!(
        match error.kind() {
            ErrorKind::LibraryError(error) => {
                !expected.starts_with("bad argument") && error.to_string() == expected
            }
            ErrorKind::ArgError(argument) => {
                expected.starts_with("bad argument") && argument.to_string() == expected
            }
            _ => false,
        },
        "expected format error {expected:?}, got {error:?}"
    );
}

#[test]
fn upper_fixed_conversion_is_rejected_like_lua_54() {
    // Reference lstrlib (5.4.9 checked) has no %F; 5.2 rejects it as well.
    expect_runtime_error(
        "string.format('%F', 1.5)",
        "invalid conversion '%F' to 'format'",
    );
    expect_runtime_error(
        "string.format('%5.2F', 1.5)",
        "invalid conversion '%5.2F' to 'format'",
    );
}

#[test]
fn string_conversion_honours_the_shared_tostring_handler() {
    assert_eq!(
        support::eval_bytes(
            r#"
            local mt = getmetatable("")
            mt.__tostring = function(s) return "[" .. s .. "]" end
            local result = string.format("%s|%6.2s", "abc", "def")
            mt.__tostring = nil
            return result .. "|" .. string.format("%s", "plain")
            "#
        ),
        b"[abc]|    [d|plain"
    );
    assert_eq!(
        support::eval_bytes(
            r#"
            getmetatable("").__tostring = function() return "\xff\0x" end
            return string.format("%s", "ignored")
            "#
        ),
        &[0xff, 0, b'x']
    );
    expect_runtime_error(
        r#"getmetatable("").__tostring = function() return {} end
            return string.format("%s", "ignored")"#,
        "'__tostring' must return a string",
    );
}

#[test]
fn numeric_strings_and_percent_are_supported() {
    assert_eq!(
        support::eval_bytes(r#"return string.format("%d|%.1f", "12", "1.5")"#),
        b"12|1.5"
    );
    assert_eq!(
        support::eval_bytes(r#"return string.format("100%%")"#),
        b"100%"
    );
    assert_eq!(support::eval_bytes(r#"return string.format(12)"#), b"12");
}

#[test]
fn byte_strings_remain_byte_exact() {
    assert_eq!(
        support::eval_bytes(
            r#"
            return string.format(
                "%c%c",
                256,
                -1
            )
            "#
        ),
        &[0x00, 0xff]
    );
    assert_eq!(
        support::eval_bytes(
            r#"
            return string.format(
                "%q",
                string.format("%c%c%c%c%c%c", 97, 34, 92, 10, 1, 50)
            )
            "#
        ),
        b"\"a\\\"\\\\\\\n\\0012\""
    );

    let mut state = State::new();
    support::eval(
        &mut state,
        r#"
            local value = string.format("%c%c%c", 97, 0, 98)
            return string.len(string.format("%s", value))
            "#,
        1,
    );
    assert_eq!(state.to_number(-1).expect("test returns a number"), 3.0);
}

#[test]
fn quoted_floats_round_trip_through_the_parser() {
    // %q emits numbers as hex floats; the lexer accepts that syntax, so
    // dellingr can re-parse its own %q output to the identical value.
    for source in ["1.5", "-0.25", "3.141592653589793", "1e-300", "0.1", "1024"] {
        let quoted = support::eval_bytes(format!(r#"return string.format("%q", {source})"#));
        let literal = String::from_utf8(quoted).expect("numeric %q output is ASCII");
        let check = format!("return ({literal}) == ({source})");
        let mut state = State::new();
        support::eval(&mut state, &check, 1);
        assert!(
            state.to_boolean(-1),
            "%q of {source} did not round-trip: {literal}"
        );
    }
}

#[test]
fn quoted_literals_cover_nil_and_reject_non_literals() {
    assert_eq!(
        support::eval_bytes(r#"return string.format("%q", nil)"#),
        b"nil"
    );
    expect_runtime_error(
        r#"return string.format("%q", {})"#,
        "bad argument #2 to 'format' (value has no literal form)",
    );
}

#[test]
fn pointer_format_is_deterministic_and_identity_based() {
    assert_eq!(
        support::eval_bytes(r#"return string.format("%p", nil)"#),
        b"(null)"
    );
    assert_eq!(
        support::eval_bytes(r#"return string.format("%p|%p|%p", print, print, type)"#),
        b"0x1|0x1|0x2"
    );
    let output = support::eval_bytes(
        r#"
        local a, b = {}, {}
        return string.format("%p|%p|%p|%p", a, a, b, "interned")
        "#,
    );
    let fields: Vec<&[u8]> = output.split(|byte| *byte == b'|').collect();
    assert_eq!(fields.len(), 4);
    assert_eq!(fields[0], fields[1]);
    assert_ne!(fields[0], fields[2]);
    assert_ne!(fields[2], fields[3]);
    assert!(
        fields
            .iter()
            .all(|field| field.starts_with(b"0x") && field[2..].iter().all(u8::is_ascii_hexdigit))
    );
}

#[test]
fn format_errors_have_exact_lua_54_payloads() {
    for (code, expected) in [
        (
            r#"return string.format("%d")"#,
            "bad argument #2 to 'format' (no value)",
        ),
        (
            r#"return string.format("%d", "x")"#,
            "bad argument #2 to 'format' (number expected, got string)",
        ),
        (
            r#"return string.format("%d", {})"#,
            "bad argument #2 to 'format' (number expected, got table)",
        ),
        (
            r#"return string.format("%d", 1.5)"#,
            "bad argument #2 to 'format' (number has no integer representation)",
        ),
        (
            r#"return string.format("%y", 1)"#,
            "invalid conversion '%y' to 'format'",
        ),
        (
            r#"return string.format("%100d", 1)"#,
            "invalid conversion specification: '%100d'",
        ),
        (
            r#"return string.format("%.999f", 1)"#,
            "invalid conversion specification: '%.999f'",
        ),
        (
            r#"return string.format("%#d", 1)"#,
            "invalid conversion specification: '%#d'",
        ),
        (
            r#"return string.format("%10q", "x")"#,
            "specifier '%q' cannot have modifiers",
        ),
        (
            r#"
            local value = string.format("%c%c%c", 97, 0, 98)
            return string.format("%3s", value)
            "#,
            "bad argument #2 to 'format' (string contains zeros)",
        ),
        (
            r#"return string.format("%ld", 1)"#,
            "invalid conversion '%l' to 'format'",
        ),
        (
            r#"return string.format("%5%", 1)"#,
            "invalid conversion '%5%' to 'format'",
        ),
        (
            r#"return string.format("%0000000000000000000000d", 1)"#,
            "invalid format (too long)",
        ),
        (
            r#"return string.format()"#,
            "bad argument #1 to 'format' (string expected, got no value)",
        ),
        (
            r#"return string.format(true)"#,
            "bad argument #1 to 'format' (string expected, got boolean)",
        ),
    ] {
        expect_runtime_error(code, expected);
    }
}

#[test]
fn missing_argument_wins_over_invalid_directive() {
    expect_runtime_error(
        r#"return string.format("%100d")"#,
        "bad argument #2 to 'format' (no value)",
    );
}

#[test]
fn tostring_errors_propagate_without_format_wrapping() {
    let mut state = State::new();
    support::load(
        &mut state,
        r#"
        local value = setmetatable({}, {
            __tostring = function() error("format tostring boom") end
        })
        return string.format("%s", value)
        "#,
    );
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("__tostring must fail");
    assert!(
        matches!(error.kind(), ErrorKind::ScriptError { value: ErrorValue::String(message), .. } if message == b"format tostring boom"),
        "format must propagate the original error unchanged, got {error:?}"
    );
}

#[test]
fn tostring_and_print_reject_non_string_metamethod_results() {
    for expression in [
        "tostring(value)",
        "print(value)",
        "string.format('%s', value)",
        "error(value)",
    ] {
        for result in ["{}", "true", "print", "function() end"] {
            expect_runtime_error(
                &format!(
                    r#"
                    local value = setmetatable({{}}, {{
                        __tostring = function() return {result} end
                    }})
                    {expression}
                    "#
                ),
                "'__tostring' must return a string",
            );
        }
    }
}

#[test]
fn tostring_accepts_numeric_metamethod_results() {
    assert_eq!(
        support::eval_bytes(
            r#"
            local value = setmetatable({}, { __tostring = function() return 42 end })
            return tostring(value)
            "#
        ),
        b"42"
    );
}

#[test]
fn tostring_object_identity_matches_pointer_format() {
    let rendered = support::eval_bytes(
        r#"
        local table_value = {}
        local function_value = function() end
        return tostring(table_value) .. "|" .. tostring(table_value)
            .. "|" .. tostring(function_value) .. "|" .. string.format("%p", table_value)
            .. "|" .. string.format("%p", function_value)
        "#,
    );
    let rendered = String::from_utf8(rendered).expect("rendering is UTF-8");
    let fields: Vec<_> = rendered.split('|').collect();
    assert_eq!(fields.len(), 5);
    assert_eq!(fields[0], fields[1]);
    assert!(fields[0].starts_with("table: 0x"));
    assert!(fields[2].starts_with("function: 0x"));
    assert!(!rendered.contains("ObjectKey"));
    assert_eq!(fields[0].strip_prefix("table: "), Some(fields[3]));
    assert_eq!(fields[2].strip_prefix("function: "), Some(fields[4]));
}

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

#[test]
fn tostring_metamethod_must_return_a_string_like_value() {
    expect_runtime_error(
        r#"
        local value = setmetatable({}, {
            __tostring = function() return {} end
        })
        return string.format("%s", value)
        "#,
        "'__tostring' must return a string",
    );
}
