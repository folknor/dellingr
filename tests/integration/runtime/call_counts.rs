use dellingr::error::ErrorKind;
use dellingr::limits::{MAX_ARGUMENTS, MAX_RESULTS};
use dellingr::{ArgCount, RetCount, State};

use crate::support;

fn expect_error(code: &str, expected: &str, kind_matches: fn(&ErrorKind) -> bool) {
    let mut state = State::new();
    support::load(&mut state, code);
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
    matches!(
        kind,
        ErrorKind::TooManyArguments {
            limit: MAX_ARGUMENTS
        }
    )
}

fn too_many_results(kind: &ErrorKind) -> bool {
    matches!(kind, ErrorKind::TooManyResults { limit: MAX_RESULTS })
}

fn arg_error(kind: &ErrorKind) -> bool {
    matches!(kind, ErrorKind::ArgError(_))
}

#[test]
fn dynamic_call_accepts_max_arguments() {
    // Packing into a table counts arguments without adding another call argument.
    let count = support::eval_number(
        r##"
        local t = {}
        for i = 1, LIMIT do t[i] = i end
        local function f(...) local packed = {...} return #packed end
        return f(table.unpack(t))
    "##
        .replace("LIMIT", &MAX_ARGUMENTS.to_string()),
    );
    assert_eq!(count, f64::from(MAX_ARGUMENTS));
}

#[test]
fn dynamic_call_rejects_arguments_above_the_cap() {
    expect_error(
        &r##"
        local t = {}
        for i = 1, LIMIT do t[i] = i end
        local function f(...) return select("#", ...) end
        return f(0, table.unpack(t))
    "##
        .replace("LIMIT", &MAX_ARGUMENTS.to_string()),
        "too many arguments (limit 255)",
        too_many_arguments,
    );
}

#[test]
fn lua_return_rejects_results_above_the_cap() {
    expect_error(
        &r#"
        local t = {}
        for i = 1, LIMIT do t[i] = i end
        local function g() return 0, table.unpack(t) end
        return g()
    "#
        .replace("LIMIT", &MAX_RESULTS.to_string()),
        "too many results (limit 255)",
        too_many_results,
    );
}

#[test]
fn callable_table_rejects_receiver_above_the_argument_cap() {
    expect_error(
        &r##"
        local t = {}
        for i = 1, LIMIT do t[i] = i end
        local callable = setmetatable({}, {
            __call = function(...) return select("#", ...) end,
        })
        return callable(table.unpack(t))
    "##
        .replace("LIMIT", &MAX_ARGUMENTS.to_string()),
        "too many arguments (limit 255)",
        too_many_arguments,
    );
}

#[test]
fn unpack_rejects_oversized_range() {
    for code in [
        r#"
        local t = {}
        for i = 1, OVER_LIMIT do t[i] = i end
        return unpack(t)
        "#,
        r#"
        local t = {}
        for i = 1, OVER_LIMIT do t[i] = i end
        return table.unpack(t)
        "#,
    ] {
        let code = code.replace("OVER_LIMIT", &(usize::from(MAX_RESULTS) + 1).to_string());
        expect_error(&code, "too many results (limit 255)", too_many_results);
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
