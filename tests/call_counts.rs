use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

fn run_number(code: &str) -> f64 {
    let mut state = State::new();
    dellingr::Program::compile(code, None)
        .and_then(|program| state.load(&program))
        .unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .unwrap_or_else(|e| panic!("Error running: {code}\n{e}"));
    state.to_number(-1).unwrap()
}

fn expect_error(code: &str, expected: &str, kind_matches: fn(&ErrorKind) -> bool) {
    let mut state = State::new();
    dellingr::Program::compile(code, None)
        .and_then(|program| state.load(&program))
        .unwrap();
    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("script must return an error");
    assert!(
        kind_matches(err.kind()),
        "unexpected error kind: {:?}",
        err.kind()
    );
    assert_eq!(err.kind().to_string(), expected);
}

fn too_many_arguments(kind: &ErrorKind) -> bool {
    matches!(kind, ErrorKind::TooManyArguments { limit: 255 })
}

fn too_many_results(kind: &ErrorKind) -> bool {
    matches!(kind, ErrorKind::TooManyResults { limit: 255 })
}

fn arg_error(kind: &ErrorKind) -> bool {
    matches!(kind, ErrorKind::ArgError(_))
}

#[test]
fn dynamic_call_accepts_255_arguments() {
    // f receives 255 dynamic arguments; it must NOT itself re-expand them into
    // another call (e.g. select("#", ...) would be a 256-arg call and hit the
    // limit). Packing into a table and taking its length counts them safely.
    let count = run_number(
        r##"
        local t = {}
        for i = 1, 255 do t[i] = i end
        local function f(...) local packed = {...} return #packed end
        return f(table.unpack(t))
    "##,
    );
    assert_eq!(count, 255.0);
}

#[test]
fn dynamic_call_rejects_256_arguments() {
    expect_error(
        r##"
        local t = {}
        for i = 1, 255 do t[i] = i end
        local function f(...) return select("#", ...) end
        return f(0, table.unpack(t))
    "##,
        "too many arguments (limit 255)",
        too_many_arguments,
    );
}

#[test]
fn lua_return_rejects_256_results() {
    expect_error(
        r#"
        local t = {}
        for i = 1, 255 do t[i] = i end
        local function g() return 0, table.unpack(t) end
        return g()
    "#,
        "too many results (limit 255)",
        too_many_results,
    );
}

#[test]
fn callable_table_rejects_implicit_256th_argument() {
    expect_error(
        r##"
        local t = {}
        for i = 1, 255 do t[i] = i end
        local callable = setmetatable({}, {
            __call = function(...) return select("#", ...) end,
        })
        return callable(table.unpack(t))
    "##,
        "too many arguments (limit 255)",
        too_many_arguments,
    );
}

#[test]
fn unpack_rejects_oversized_range() {
    for code in [
        r#"
        local t = {}
        for i = 1, 256 do t[i] = i end
        return unpack(t)
        "#,
        r#"
        local t = {}
        for i = 1, 256 do t[i] = i end
        return table.unpack(t)
        "#,
    ] {
        expect_error(code, "too many results (limit 255)", too_many_results);
    }
}

#[test]
fn unpack_rejects_huge_range_without_overflow() {
    // A huge explicit endpoint must be rejected before range arithmetic.
    for code in [
        "return unpack({}, 0, 1e300)",
        "return table.unpack({}, 0, 1e300)",
    ] {
        expect_error(
            code,
            "bad argument #3 to 'unpack' (number has no integer representation)",
            arg_error,
        );
    }
}

#[test]
fn unpack_rejects_i64_span_overflow() {
    // The key-range refusal now fires before the result-count check.
    for (code, name) in [
        (
            "return unpack({}, -9223372036854775808, 9223372036854774784)",
            "unpack",
        ),
        (
            "return table.unpack({}, -9223372036854775808, 9223372036854774784)",
            "unpack",
        ),
    ] {
        expect_error(
            code,
            &format!("{name} range exceeds the exactly representable integer keys"),
            |kind| matches!(kind, ErrorKind::LibraryError(_)),
        );
    }
    // In-range but too wide still reports the result limit.
    expect_error(
        "return table.unpack({}, -9007199254740992, 9007199254740992)",
        "too many results (limit 255)",
        too_many_results,
    );
}
