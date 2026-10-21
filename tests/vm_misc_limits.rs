//! Runtime value-count cap, `error()` size preflight and scope naming.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State, analyze_cost};

#[test]
fn lua_return_of_many_values_hits_documented_cap() {
    let mut state = State::new();
    state
        .load_string(
            r#"
        local t = {}
        for i = 1, 255 do t[i] = i end
        local function many(...) return 0, ... end
        return many(table.unpack(t))
    "#,
        )
        .unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::All)
        .expect_err("256 Lua results exceed the runtime cap");
    assert!(
        matches!(error.kind(), ErrorKind::TooManyResults { limit: 255 }),
        "unexpected error: {error:?}"
    );
}

#[test]
fn error_with_expanding_invalid_bytes_is_size_checked() {
    let mut state = State::new();
    // 2^23 bytes of an invalid UTF-8 byte: under the 16 MiB cap raw, but each
    // byte becomes a 3-byte replacement character, so the converted message
    // would be about 24 MiB.
    state
        .load_string(
            r#"
        local s = "\255"
        for i = 1, 23 do s = s .. s end
        error(s)
    "#,
        )
        .unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("error() must fail");
    assert!(
        matches!(error.kind(), ErrorKind::StringSizeExceeded { size, limit }
            if *size == 3 * (1 << 23) && *limit == dellingr::limits::MAX_STRING_BYTES),
        "the oversized message must be rejected, got {error:?}"
    );
}

#[test]
fn error_with_small_invalid_bytes_still_raises_script_error() {
    let mut state = State::new();
    state.load_string(r#"error("a\255b")"#).unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("error() must fail");
    assert!(
        matches!(error.kind(), ErrorKind::ScriptError { message, .. } if message == "a\u{FFFD}b"),
        "unexpected error: {error:?}"
    );
}

#[test]
fn anonymous_scopes_are_numbered_among_themselves() {
    let analysis = analyze_cost(
        "local f = print\n\
         f(function() end)\n\
         local function named() end\n\
         f(function() end)\n",
    )
    .unwrap();
    let anonymous: Vec<&str> = analysis
        .root
        .nested
        .iter()
        .map(|scope| scope.name.as_str())
        .filter(|name| name.starts_with("anonymous"))
        .collect();
    assert_eq!(anonymous, ["anonymous #1", "anonymous #2"]);
}

#[test]
fn free_scopes_display_call_instruction_counts() {
    let analysis = analyze_cost("print()").unwrap();
    assert_eq!(analysis.root.own_cost, 0);
    assert_eq!(analysis.root.function_calls(), 1);
    let rendered = analysis.to_string();
    let (_, scopes) = rendered.split_once("--- Per-Scope Breakdown ---").unwrap();
    assert!(scopes.contains("call instructions: 1"), "{rendered}");
    assert!(rendered.contains("Call instructions:    1"), "{rendered}");
}
