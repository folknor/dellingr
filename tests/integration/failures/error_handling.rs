//! Tests for proper error handling (things that used to panic).
//!
//! These tests verify that the VM returns clean errors instead of panicking
//! when it encounters unexpected types or corrupt state.

use dellingr::error::ErrorKind;
use dellingr::limits::MAX_STRING_BYTES;
use dellingr::{ArgCount, LuaType, RetCount, State};

use crate::support::PushNewRustFn;
use crate::support::{self, run_error, stack};

fn assert_pattern_error(code: &str, message: &str) {
    let err = run_error(code);
    match err.kind() {
        ErrorKind::PatternError(detail) => assert_eq!(detail.to_string(), message),
        kind => panic!("Expected pattern error, got: {kind:?}"),
    }
}

fn assert_trace_lines(code: &str, innermost: u32, caller: u32) {
    let mut state = State::new();
    dellingr::Program::compile(code, Some(b"trace".as_slice()))
        .and_then(|program| state.load(&program))
        .unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("trace program must fail");
    assert_eq!(error.stack_trace()[0].line, innermost);
    assert_eq!(error.stack_trace()[1].line, caller);
    let rendered = format!("{error}");
    assert!(rendered.contains(&format!("trace:{innermost}:")));
    assert!(rendered.contains(&format!("trace:{caller}:")));
}

#[test]
fn traceback_call_sites_cover_non_call_dispatch_and_host_calls() {
    assert_trace_lines(
        "local function iter()\n  error('boom')\nend\nfor value in iter do end",
        2,
        4,
    );
    assert_trace_lines(
        "local value = setmetatable({}, {\n  __index = function()\n    error('boom')\n  end,\n})\nlocal result = value.field",
        3,
        6,
    );
    assert_trace_lines(
        "local value = setmetatable({}, {\n  __newindex = function()\n    error('boom')\n  end,\n})\nvalue.field = 1",
        3,
        6,
    );
    assert_trace_lines(
        "local value = setmetatable({}, {\n  __len = function()\n    error('boom')\n  end,\n})\nlocal result = #value",
        3,
        6,
    );
    // The handler fails during the method lookup, before the call that
    // follows it could record its own site.
    assert_trace_lines(
        "local value = setmetatable({}, {\n  __index = function()\n    error('boom')\n  end,\n})\nvalue:method()",
        3,
        6,
    );
    // A call spanning several lines is attributed to its opening line.
    assert_trace_lines(
        "local value = {2, 1}\ntable.sort(value, function()\n  error('boom')\nend)",
        3,
        2,
    );
    assert_trace_lines(
        "local value = setmetatable({}, {\n  __tostring = function()\n    error('boom')\n  end,\n})\nlocal result = tostring(value)",
        3,
        6,
    );
}

/// A Rust handler (the builtin `tostring`) between the table or length
/// instruction and the failing Lua `__tostring` has no activation record of
/// its own, so the caller entry is the instruction's recorded call site. The
/// `setmetatable` call on line 1 is the previous recorded site.
#[test]
fn traceback_call_sites_cross_rust_handlers() {
    let setup = "local value = setmetatable({}, {\n  __index = tostring,\n  __newindex = tostring,\n  __len = tostring,\n  __tostring = function()\n    error('boom')\n  end,\n})\n";
    for (name, statement) in [
        ("GET_FIELD", "local result = value.field"),
        ("SELF", "value:method()"),
        (
            "GET_TABLE",
            "local key = 'field'\nlocal result = value[key]",
        ),
        ("SET_FIELD", "value.field = 1"),
        ("SET_TABLE", "local key = 'field'\nvalue[key] = 1"),
        ("LEN", "local result = #value"),
    ] {
        let code = format!("{setup}{statement}");
        let line = u32::try_from(code.lines().count()).expect("the source is short");
        let source = Some(b"trace".as_slice());
        let mut state = State::new();
        dellingr::Program::compile(&code, source)
            .and_then(|program| state.load(&program))
            .unwrap();
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("trace program must fail");
        let trace = error.stack_trace();
        assert_eq!(trace.len(), 2, "{name}: {trace:?}");
        assert_eq!(trace[0].line, 6, "{name}: {trace:?}");
        assert_eq!(trace[1].line, line, "{name}: {trace:?}");
    }
}

#[test]
fn malformed_pattern_capture_errors_are_pattern_errors() {
    assert_pattern_error(r#"string.match("aa", "(a)%0")"#, "invalid capture index %0");
    assert_pattern_error(r#"string.match("a", ")")"#, "invalid pattern capture");
}

#[test]
fn call_rejects_missing_fixed_arguments_without_panicking() {
    let mut state = State::new();
    state.push_new_rust_fn(|_state| Ok(0)).unwrap();

    let err = state
        .call(ArgCount::Fixed(1), RetCount::Fixed(0))
        .expect_err("missing fixed argument must return an error");
    assert!(matches!(
        err.kind(),
        ErrorKind::InvalidStackIndex { index: -2 }
    ));
    assert_eq!(state.get_top(), 1);
    assert_eq!(state.typ(-1), LuaType::Function);
}

#[test]
fn public_call_rejects_dynamic_without_base_without_panicking() {
    let mut state = State::new();
    state.push_new_rust_fn(|_state| Ok(0)).unwrap();

    let err = state
        .call(ArgCount::Dynamic, RetCount::Fixed(0))
        .expect_err("host Dynamic call without a base must return an error");
    assert!(matches!(
        err.kind(),
        ErrorKind::DynamicArgumentsUnsupported {
            api: dellingr::error::HostCallApi::Call
        }
    ));
    assert_eq!(state.get_top(), 1);
    assert_eq!(state.typ(-1), LuaType::Function);
}

// -- Error type tests --

#[test]
fn error_function_produces_error() {
    let err = run_error("error('user error message')");
    let msg = format!("{err}");
    assert!(
        msg.contains("user error message"),
        "Error should contain user message, got: {msg}"
    );
}

#[test]
fn error_level_selects_or_suppresses_only_the_prefix() {
    let default = run_error("error('boom')");
    assert!(format!("{default}").starts_with("1:0: boom"));

    let zero = run_error("error('boom', 0)");
    assert!(format!("{zero}").starts_with("boom\nstack traceback:"));

    let one = run_error("error('boom', 1)");
    assert!(format!("{one}").starts_with("1:0: boom"));

    let two = run_error("local function f() error('boom', 2) end\nf()");
    assert_eq!(two.stack_trace().len(), 2);
    assert!(format!("{two}").starts_with("2:0: boom"));

    let out_of_range = run_error("error('boom', 99)");
    assert!(format!("{out_of_range}").starts_with("boom\nstack traceback:"));

    let non_integer = run_error("error('boom', 1.5)");
    assert!(format!("{non_integer}").contains("number has no integer representation"));
}

#[test]
fn type_error_on_arithmetic() {
    let err = run_error("local x = 'hello' + 1");
    assert!(
        matches!(err.kind(), ErrorKind::TypeError(_)),
        "Expected TypeError, got: {err}"
    );
}

#[test]
fn type_error_on_call() {
    let err = run_error("local x = 5\nx()");
    assert!(
        matches!(err.kind(), ErrorKind::TypeError(_)),
        "Expected TypeError, got: {err}"
    );
}

#[test]
fn type_error_on_index() {
    let err = run_error("local x = 5\nlocal y = x.foo");
    assert!(
        matches!(err.kind(), ErrorKind::TypeError(_)),
        "Expected TypeError, got: {err}"
    );
}

#[test]
fn type_error_on_table_key_nil() {
    let err = run_error("local t = {}\nt[nil] = 1");
    assert!(
        matches!(err.kind(), ErrorKind::TypeError(_)),
        "Expected TypeError, got: {err}"
    );
}

#[test]
fn budget_exceeded_error() {
    let mut state = State::new();
    state.set_cost_budget(10);
    support::load(
        &mut state,
        r#"
        local sum = 0
        for i = 1, 10000 do
            sum = sum + i
        end
    "#,
    );
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    let err = result.expect_err("Expected budget error");
    assert!(
        matches!(err.kind(), ErrorKind::BudgetExceeded { .. }),
        "Expected BudgetExceeded, got: {err}"
    );
}

#[test]
fn budget_stops_at_the_first_operation_after_exhaustion() {
    for budget in [1, 63, 65] {
        let mut state = State::new();
        state.set_cost_budget(budget);
        let increments = budget + 2;
        support::load(
            &mut state,
            format!("x = 0\n{}", "x = x + 1\n".repeat(increments as usize)),
        );

        let err = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("the operation after the exhausted budget must fail");
        assert!(matches!(err.kind(), ErrorKind::BudgetExceeded { .. }));

        state.get_global("x").unwrap();
        assert_eq!(
            state.to_number(-1).expect("x should be numeric"),
            budget as f64
        );
        assert_eq!(state.cost_used(), budget as u64);
        assert_eq!(state.cost_remaining(), budget - budget);
    }
}

#[test]
fn budget_flushes_pending_caller_cost_before_nested_call() {
    let mut state = State::new();
    state.set_cost_budget(1);
    support::load(
        &mut state,
        "x = 0\nlocal function f() x = x + 1 end\nx = x + 1\nf()\nx = x + 1",
    );

    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("callee must observe the caller's pending cost");
    assert!(matches!(err.kind(), ErrorKind::BudgetExceeded { .. }));
    state.get_global("x").unwrap();
    assert_eq!(state.to_number(-1).expect("x should be numeric"), 1.0);
    assert_eq!(state.cost_used(), 1);
    assert_eq!(state.cost_remaining(), 0);
}

// -- Error message quality tests --

/// Helper: tries to load+call Lua code and returns the error, panicking if it succeeds.
/// Discards results since we only care about the error.
fn expect_load_or_run_error(code: &str) -> dellingr::error::Error {
    let mut state = State::new();
    if let Err(e) = dellingr::Program::compile(code, None).and_then(|program| state.load(&program))
    {
        return e;
    }
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    result.expect_err(&format!("Expected error from: {code}"))
}

#[test]
fn error_msg_unexpected_token_includes_context() {
    // Using 'end' where an expression is expected
    let err = expect_load_or_run_error("local x = end");
    assert_eq!(err.kind().to_string(), "unexpected symbol near 'end'");
}

#[test]
fn error_msg_vararg_outside_vararg_function() {
    // '...' inside a non-vararg function should error
    let err = expect_load_or_run_error("local function f() return ... end");
    assert_eq!(
        err.kind().to_string(),
        "cannot use '...' outside a vararg function"
    );
}

#[test]
fn error_msg_vararg_in_table_outside_vararg_function() {
    let err = expect_load_or_run_error("local function f() return {...} end");
    assert_eq!(
        err.kind().to_string(),
        "cannot use '...' outside a vararg function"
    );
}

#[test]
fn error_msg_missing_end_keyword() {
    let err = expect_load_or_run_error("if true then local x = 1");
    assert_eq!(err.kind().to_string(), "unexpected <eof>");
}

#[test]
fn error_msg_type_error_arithmetic() {
    let err = run_error("local x = 'hello' + 1");
    assert_eq!(
        err.kind().to_string(),
        "attempt to perform arithmetic on a string value"
    );
}

#[test]
fn error_msg_type_error_call() {
    let err = run_error("local x = 5\nx()");
    assert_eq!(err.kind().to_string(), "attempt to call a number value");
}

#[test]
fn error_msg_type_error_index() {
    let err = run_error("local x = 5\nlocal y = x.foo");
    assert_eq!(err.kind().to_string(), "attempt to index a number value");
}

#[test]
fn error_msg_budget_exceeded_shows_amounts() {
    let mut state = State::new();
    state.set_cost_budget(10);
    support::load(&mut state, "local s = 0\nfor i = 1, 10000 do s = s + i end");
    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("Expected budget error");
    assert_eq!(
        err.kind().to_string(),
        format!(
            "budget exceeded: used {} cost with budget of 10",
            state.cost_used()
        )
    );
}

#[test]
fn error_msg_call_depth_shows_overflow() {
    stack::on_limit_test_stack("error_msg_call_depth_shows_overflow", || {
        let err = run_error("local function r(n) return r(n+1) end\nr(0)");
        let limit = dellingr::limits::MAX_CALL_DEPTH;
        assert_eq!(
            err.kind().to_string(),
            format!("call stack overflow (depth {limit}, limit {limit})")
        );
    });
}

/// Lua -> RustFn -> Lua recursion keeps a Rust callback frame and a fresh
/// dispatch loop live on the native stack per level, so it is bounded by the
/// native call limit, not by the Lua call depth.
#[test]
fn native_call_depth_exceeded_through_rust_callbacks() {
    stack::on_limit_test_stack("native_call_depth_exceeded_through_rust_callbacks", || {
        let err = run_error(
            r#"
            local function recurse(n)
                local t = {2, 1}
                table.sort(t, function(a, b)
                    recurse(n + 1)
                    return a < b
                end)
            end
            recurse(0)
        "#,
        );
        match err.kind() {
            ErrorKind::NativeCallDepthExceeded { depth, limit } => {
                assert_eq!(*depth, dellingr::limits::MAX_NATIVE_CALL_DEPTH);
                assert_eq!(*limit, dellingr::limits::MAX_NATIVE_CALL_DEPTH);
            }
            other => panic!("Expected NativeCallDepthExceeded, got: {other}"),
        }
    });
}

#[test]
fn host_string_limit_is_inclusive_and_leaves_stack_unchanged() {
    let mut state = State::new();
    state
        .push_bytes(vec![b'x'; MAX_STRING_BYTES])
        .expect("the exact string limit must succeed");
    let err = state
        .push_bytes(vec![b'x'; MAX_STRING_BYTES + 1])
        .expect_err("a string above the limit must fail");
    match err.kind() {
        ErrorKind::StringSizeExceeded { size, limit } => {
            assert_eq!(*size, MAX_STRING_BYTES + 1);
            assert_eq!(*limit, MAX_STRING_BYTES);
        }
        kind => panic!("expected StringSizeExceeded, got: {kind:?}"),
    }
    assert_eq!(state.get_top(), 1);
}

#[test]
fn call_depth_exceeded_error() {
    stack::on_limit_test_stack("call_depth_exceeded_error", || {
        // Rejected with exactly MAX_CALL_DEPTH activations live: every call
        // up to the limit was admitted, and only the next one refused.
        let err = run_error(
            r#"
            local function recurse(n)
                return recurse(n + 1)
            end
            recurse(0)
        "#,
        );
        match err.kind() {
            ErrorKind::CallDepthExceeded { depth, limit } => {
                assert_eq!(*depth, dellingr::limits::MAX_CALL_DEPTH);
                assert_eq!(*limit, dellingr::limits::MAX_CALL_DEPTH);
            }
            other => panic!("Expected CallDepthExceeded, got: {other}"),
        }
    });
}
