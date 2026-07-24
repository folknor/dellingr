use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

fn run_number(code: &str) -> f64 {
    let mut state = State::new();
    state.load_string(code).unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .unwrap_or_else(|e| panic!("Error running: {code}\n{e}"));
    state.to_number(-1).unwrap()
}

fn expect_runtime_error(code: &str, expected: &str) {
    let mut state = State::new();
    state.load_string(code).unwrap();
    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("script must return an error");
    assert!(
        matches!(err.kind, ErrorKind::RuntimeError(ref message) if message == expected),
        "expected RuntimeError({expected:?}), got: {err}"
    );
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
    expect_runtime_error(
        r##"
        local t = {}
        for i = 1, 255 do t[i] = i end
        local function f(...) return select("#", ...) end
        return f(0, table.unpack(t))
    "##,
        "too many arguments (limit 255)",
    );
}

#[test]
fn lua_return_rejects_256_results() {
    expect_runtime_error(
        r#"
        local t = {}
        for i = 1, 255 do t[i] = i end
        local function g() return 0, table.unpack(t) end
        return g()
    "#,
        "too many results (limit 255)",
    );
}

#[test]
fn callable_table_rejects_implicit_256th_argument() {
    expect_runtime_error(
        r##"
        local t = {}
        for i = 1, 255 do t[i] = i end
        local callable = setmetatable({}, {
            __call = function(...) return select("#", ...) end,
        })
        return callable(table.unpack(t))
    "##,
        "too many arguments (limit 255)",
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
        expect_runtime_error(code, "too many results to unpack");
    }
}

#[test]
fn unpack_rejects_huge_range_without_overflow() {
    // A huge explicit `j` must be rejected by the span bound, not overflow the
    // count arithmetic (which would panic in debug and loop unbounded in release).
    for code in [
        "return unpack({}, 0, 1e300)",
        "return table.unpack({}, 0, 1e300)",
    ] {
        expect_runtime_error(code, "too many results to unpack");
    }
}
