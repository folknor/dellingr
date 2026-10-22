//! Runtime value-count cap, typed `error()` values and scope naming.

use dellingr::error::{ErrorKind, ErrorValue};
use dellingr::{ArgCount, LuaType, Program, RetCount, State};

#[test]
fn lua_return_of_many_values_hits_documented_cap() {
    let mut state = State::new();
    dellingr::Program::compile(
        r#"
        local t = {}
        for i = 1, 255 do t[i] = i end
        local function many(...) return 0, ... end
        return many(table.unpack(t))
    "#,
        None,
    )
    .and_then(|program| state.load(&program))
    .unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::All)
        .expect_err("256 Lua results exceed the runtime cap");
    assert!(
        matches!(error.kind(), ErrorKind::TooManyResults { limit: 255 }),
        "unexpected error: {error:?}"
    );
}

fn raise(source: &str) -> dellingr::error::Error {
    let mut state = State::new();
    let program = Program::compile(source, None).expect("error fixture compiles");
    state.load(&program).expect("error fixture loads");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("error() must fail")
}

#[test]
fn error_with_large_invalid_bytes_carries_them_exactly() {
    // 2^23 bytes of an invalid UTF-8 byte: under the 16 MiB cap. No lossy
    // conversion happens, so nothing expands past the cap.
    let error = raise(
        r#"
        local s = "\255"
        for i = 1, 23 do s = s .. s end
        error(s)
    "#,
    );
    assert!(
        matches!(error.kind(), ErrorKind::ScriptError { value: ErrorValue::String(bytes), .. }
            if bytes.len() == 1 << 23 && bytes.iter().all(|byte| *byte == 0xff)),
        "the script's bytes must reach the host"
    );
}

#[test]
fn error_with_small_invalid_bytes_raises_exact_bytes() {
    let error = raise(r#"error("a\255b")"#);
    assert!(
        matches!(error.kind(), ErrorKind::ScriptError { value: ErrorValue::String(bytes), .. }
            if bytes == b"a\xffb"),
        "unexpected error: {error:?}"
    );
    // Display stays diagnostic text, rendered lossily.
    assert!(error.to_string().contains(": a\u{FFFD}b\n"), "{error}");
}

#[test]
fn error_values_are_typed() {
    for (source, expected, rendered) in [
        ("error()", ErrorValue::Nil, "nil"),
        ("error(nil)", ErrorValue::Nil, "nil"),
        ("error(true)", ErrorValue::Boolean(true), "true"),
        ("error(42)", ErrorValue::Number(42.0), "42"),
        ("error(0.5)", ErrorValue::Number(0.5), "0.5"),
        (
            "error(setmetatable({}, {__tostring = function() return 'obj\\255' end}))",
            ErrorValue::Object {
                value_type: LuaType::Table,
                rendering: b"obj\xff".to_vec(),
            },
            "obj\u{FFFD}",
        ),
        (
            "error({})",
            ErrorValue::Object {
                value_type: LuaType::Table,
                rendering: b"table: 0x1".to_vec(),
            },
            "table: 0x1",
        ),
        (
            "error(print)",
            ErrorValue::Object {
                value_type: LuaType::Function,
                rendering: b"function: 0x1".to_vec(),
            },
            "function: 0x1",
        ),
    ] {
        let error = raise(source);
        let ErrorKind::ScriptError { value, .. } = error.kind() else {
            panic!("{source}: expected a script error, got {error:?}");
        };
        assert_eq!(*value, expected, "{source}");
        assert_eq!(value.to_string(), rendered, "{source}");
        assert_eq!(value.value_type(), expected.value_type(), "{source}");
    }
}

#[test]
fn string_error_values_bypass_the_string_tostring_handler() {
    let error = raise(
        "getmetatable('').__tostring = function() return 'wrapped' end\n\
         error('raw')",
    );
    assert!(matches!(
        error.kind(),
        ErrorKind::ScriptError { value: ErrorValue::String(bytes), .. } if bytes == b"raw"
    ));
}

#[test]
fn anonymous_scopes_are_numbered_among_themselves() {
    let analysis = dellingr::Program::compile(
        "local f = print\n\
         f(function() end)\n\
         local function named() end\n\
         f(function() end)\n",
        None,
    )
    .map(|program| program.analyze_cost())
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
    let analysis = dellingr::Program::compile("print()", None)
        .map(|program| program.analyze_cost())
        .unwrap();
    assert_eq!(analysis.root.own_cost, 0);
    assert_eq!(analysis.root.function_calls(), 1);
    let rendered = analysis.to_string();
    let (_, scopes) = rendered.split_once("--- Per-Scope Breakdown ---").unwrap();
    assert!(scopes.contains("call instructions: 1"), "{rendered}");
    assert!(rendered.contains("Call instructions:    1"), "{rendered}");
}
