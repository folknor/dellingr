use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

fn run_bytes(code: &str) -> Vec<u8> {
    let mut state = State::new();
    state
        .load_string(code)
        .expect("format test source compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("format test executes");
    state.to_bytes(-1).expect("test returns a string").to_vec()
}

fn expect_runtime_error(code: &str, expected: &str) {
    let mut state = State::new();
    state.load_string(code).expect("error test source compiles");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("format call must fail");
    assert!(
        matches!(&error.kind, ErrorKind::RuntimeError(message) if message == expected),
        "expected RuntimeError({expected:?}), got {error:?}"
    );
}

#[test]
fn integer_conversions_match_lua_54() {
    assert_eq!(
        run_bytes(r#"return string.format("%d|%i|%u|%o|%x|%X", 42, -42, 42, 10, 255, 255)"#),
        b"42|-42|42|12|ff|FF"
    );
    assert_eq!(
        run_bytes(r#"return string.format("%u|%x", -1, -1)"#),
        b"18446744073709551615|ffffffffffffffff"
    );
}

#[test]
fn decimal_float_conversions_match_lua_54() {
    assert_eq!(
        run_bytes(
            r#"return string.format("%.2e|%.2E|%.2f|%.4g|%.4G", 12.5, 12.5, 3.14159, 12, 0.000012)"#
        ),
        b"1.25e+01|1.25E+01|3.14|12|1.2E-05"
    );
    assert_eq!(
        run_bytes(r#"return string.format("[% 6.2f]|[%#.4g]", 1.5, 12)"#),
        b"[  1.50]|[12.00]"
    );
    assert_eq!(
        run_bytes(r#"return string.format("[%010f]", math.huge)"#),
        b"[       inf]"
    );
}

#[test]
fn hex_float_conversions_match_lua_54() {
    assert_eq!(
        run_bytes(r#"return string.format("%a|%A", 1.5, 1.5)"#),
        b"0x1.8p+0|0X1.8P+0"
    );
    assert_eq!(run_bytes(r#"return string.format("%q", 1.5)"#), b"0x1.8p+0");
    assert_eq!(
        run_bytes(r#"return string.format("[%#.0a]|[%.0a]", 1.5, 1.5)"#),
        b"[0x2.p+0]|[0x2p+0]"
    );
}

#[test]
fn flags_width_and_precision_match_lua_54() {
    assert_eq!(
        run_bytes(r#"return string.format("[%+06d]|[%-6d]|[%#08x]", 42, 42, 42)"#),
        b"[+00042]|[42    ]|[0x00002a]"
    );
    assert_eq!(
        run_bytes(r#"return string.format("[%5.2s]", "hello")"#),
        b"[   he]"
    );
    assert_eq!(
        run_bytes(r#"return string.format("[%#.3o]|[%08.5x]|[%+08.5d]", 1, 42, 42)"#),
        b"[001]|[   0002a]|[  +00042]"
    );
}

#[test]
fn numeric_strings_and_percent_are_supported() {
    assert_eq!(
        run_bytes(r#"return string.format("%d|%.1f", "12", "1.5")"#),
        b"12|1.5"
    );
    assert_eq!(run_bytes(r#"return string.format("100%%")"#), b"100%");
    assert_eq!(run_bytes(r#"return string.format(12)"#), b"12");
}

#[test]
fn byte_strings_remain_byte_exact() {
    assert_eq!(
        run_bytes(
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
        run_bytes(
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
    state
        .load_string(
            r#"
            local value = string.format("%c%c%c", 97, 0, 98)
            return string.len(string.format("%s", value))
            "#,
        )
        .expect("NUL test source compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("unmodified %s accepts NUL");
    assert_eq!(state.to_number(-1).expect("test returns a number"), 3.0);
}

#[test]
fn quoted_floats_round_trip_through_the_parser() {
    // C30: %q emits numbers as hex floats; the lexer accepts that syntax, so
    // dellingr can re-parse its own %q output to the identical value.
    for source in ["1.5", "-0.25", "3.141592653589793", "1e-300", "0.1", "1024"] {
        let quoted = run_bytes(&format!(r#"return string.format("%q", {source})"#));
        let literal = String::from_utf8(quoted).expect("numeric %q output is ASCII");
        let check = format!("return ({literal}) == ({source})");
        let mut state = State::new();
        state
            .load_string(&check)
            .expect("numeric %q output must re-parse");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(1))
            .expect("round-trip comparison executes");
        assert!(
            state.to_boolean(-1),
            "%q of {source} did not round-trip: {literal}"
        );
    }
}

#[test]
fn quoted_literals_cover_nil_and_reject_non_literals() {
    assert_eq!(run_bytes(r#"return string.format("%q", nil)"#), b"nil");
    expect_runtime_error(
        r#"return string.format("%q", {})"#,
        "bad argument #2 to 'format' (value has no literal form)",
    );
}

#[test]
fn pointer_format_is_deterministic_and_identity_based() {
    assert_eq!(run_bytes(r#"return string.format("%p", nil)"#), b"(null)");
    assert_eq!(
        run_bytes(r#"return string.format("%p|%p|%p", print, print, type)"#),
        b"0x1|0x1|0x2"
    );
    let output = run_bytes(
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
    state
        .load_string(
            r#"
        local value = setmetatable({}, {
            __tostring = function() error("format tostring boom") end
        })
        return string.format("%s", value)
        "#,
        )
        .expect("tostring error source compiles");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("__tostring must fail");
    assert!(
        matches!(&error.kind, ErrorKind::InternalError(message) if message == "format tostring boom"),
        "format must propagate the original error unchanged, got {error:?}"
    );
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
