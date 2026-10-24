//! The 255 argument and result limits: the accept side at the cap and the
//! typed refusal above it, for literal, dynamic, callable-table and host
//! calls and for `unpack` ranges.

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

// -- At the cap --

#[test]
fn literal_return_and_unpack_keep_all_255_values() {
    let values = (1..=255)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    for source in [
        format!("return {values}"),
        format!("return table.unpack({{{values}}})"),
    ] {
        let mut state = State::new();
        support::eval(&mut state, &source, 255);
        assert_eq!(state.get_top(), 255);
        assert_eq!(state.to_number(1).expect("first result is numeric"), 1.0);
        assert_eq!(state.to_number(255).expect("last result is numeric"), 255.0);
    }
}

#[test]
fn host_call_accepts_255_fixed_arguments() {
    let mut state = State::new();
    support::eval(&mut state, "return function(...) return ... end", 1);
    for n in 1..=255 {
        state
            .push_number(f64::from(n))
            .expect("argument fits the stack");
    }
    state
        .call(ArgCount::Fixed(255), RetCount::All)
        .expect("host count is not a bytecode sentinel");
    assert_eq!(state.get_top(), 255);
    assert_eq!(
        state.to_number(255).expect("last argument is returned"),
        255.0
    );
}

#[test]
fn callable_table_receiver_can_make_255_runtime_arguments() {
    let args = vec!["1"; 254].join(",");
    let source = format!(
        "local t = setmetatable({{}}, {{__call = function(...) local packed = {{...}}; return #packed end}}); return t({args})"
    );
    let mut state = State::new();
    support::eval(&mut state, &source, 1);
    assert_eq!(
        state.to_number(1).expect("argument count is numeric"),
        255.0
    );
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
fn runtime_result_limit_accepts_the_cap_and_rejects_overflow() {
    use dellingr::error::results_count;

    let maximum = usize::from(MAX_RESULTS);
    assert_eq!(
        results_count(maximum).expect("MAX_RESULTS fits the callback ABI"),
        MAX_RESULTS
    );
    let error = results_count(maximum + 1).expect_err("count exceeds MAX_RESULTS");
    assert!(matches!(
        error,
        ErrorKind::TooManyResults { limit: MAX_RESULTS }
    ));
    assert_eq!(error.to_string(), "too many results (limit 255)");

    for count in [maximum, maximum + 1] {
        let mut state = State::new();
        support::load(&mut state, format!("return unpack({{}}, 1, {count})"));
        let result = state.call(ArgCount::Fixed(0), RetCount::All);
        if count == maximum {
            result.expect("MAX_RESULTS results are supported");
            assert_eq!(state.get_top(), maximum);
        } else {
            let error = result.expect_err("results exceed MAX_RESULTS");
            assert!(matches!(
                error.kind(),
                ErrorKind::TooManyResults { limit: MAX_RESULTS }
            ));
            assert!(error.to_string().contains("too many results (limit 255)"));
        }
    }
}

// -- Above the cap --

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
fn runtime_argument_limit_reports_typed_errors() {
    for code in [
        "local function f(...) end; f(1, unpack({}, 1, LIMIT))",
        "local f = setmetatable({}, { __call = function(...) end }); f(unpack({}, 1, LIMIT))",
    ] {
        let mut state = State::new();
        support::load(
            &mut state,
            code.replace("LIMIT", &MAX_ARGUMENTS.to_string()),
        );
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("arguments exceed MAX_ARGUMENTS");
        assert!(matches!(
            error.kind(),
            ErrorKind::TooManyArguments {
                limit: MAX_ARGUMENTS
            }
        ));
        assert!(error.to_string().contains("too many arguments (limit 255)"));
    }
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
