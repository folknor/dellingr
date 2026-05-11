//! Stack protocol regressions for failing Rust callbacks and dynamic calls.

use dellingr::error::{Error, ErrorKind};
use dellingr::{ArgCount, LuaType, RetCount, State};

#[test]
fn set_top_accepts_negative_indices() {
    let mut state = State::new();
    state.push_number(1.0);
    state.push_number(2.0);
    state.push_number(3.0);

    state.set_top(-1);
    assert_eq!(state.get_top(), 3);
    assert_eq!(state.to_number(-1).unwrap(), 3.0);

    state.set_top(-2);
    assert_eq!(state.get_top(), 2);
    assert_eq!(state.to_number(-1).unwrap(), 2.0);

    state.set_top(-3);
    assert_eq!(state.get_top(), 0);
}

/// A zero-argument Rust function error should restore the caller's visible stack.
#[test]
fn rustfn_error_restores_stack_bottom() {
    let mut state = State::new();

    // Register a Rust function that returns an error.
    state.push_rust_fn(|_state| {
        Err(Error::without_location(ErrorKind::InternalError(
            "intentional error".to_string(),
        )))
    });
    state.set_global("error_fn");

    // Push some values to establish a stack state.
    state.push_number(1.0);
    state.push_number(2.0);

    let top_before = state.get_top();
    assert_eq!(top_before, 2, "Should have 2 values on stack");

    // Call the error function - this should fail.
    state.get_global("error_fn");
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));

    assert!(result.is_err(), "Expected error from rust fn");

    let top_after = state.get_top();

    assert_eq!(
        top_before, top_after,
        "Stack top changed after error! Before: {top_before}, After: {top_after}. stack_bottom likely corrupted."
    );
}

/// Test that stack operations work correctly after a rust_fn error.
#[test]
fn stack_operations_after_rustfn_error() {
    let mut state = State::new();

    // Register a Rust function that returns an error.
    state.push_rust_fn(|_state| {
        Err(Error::without_location(ErrorKind::InternalError(
            "intentional error".to_string(),
        )))
    });
    state.set_global("error_fn");

    // Push a known value
    state.push_number(42.0);

    // Call the error function.
    state.get_global("error_fn");
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    assert!(result.is_err(), "Expected error from rust fn");

    // Try to read the value we pushed - it should still be visible.
    let val = state.to_number(-1).expect("Stack value should be readable");
    assert_eq!(val, 42.0, "Stack value should still be 42.0");
}

#[test]
fn rustfn_error_with_arguments_clears_call_frame() {
    let mut state = State::new();

    state.push_number(10.0);
    state.push_string("host sentinel");
    let top_before = state.get_top();

    state.push_rust_fn(|state| {
        assert_eq!(state.get_top(), 3);
        assert_eq!(state.to_string(1).unwrap(), "alpha");
        assert_eq!(state.to_number(2).unwrap(), 23.0);
        assert_eq!(state.typ(3), LuaType::Boolean);
        state.push_string("callback temporary");
        Err(Error::without_location(ErrorKind::InternalError(
            "argument callback failed".to_string(),
        )))
    });
    state.push_string("alpha");
    state.push_number(23.0);
    state.push_boolean(true);

    let result = state.call(ArgCount::Fixed(3), RetCount::Fixed(0));
    assert!(result.is_err(), "Expected error from Rust callback");

    assert_eq!(state.get_top(), top_before);
    assert_eq!(state.to_number(1).unwrap(), 10.0);
    assert_eq!(state.to_string(2).unwrap(), "host sentinel");
}

#[test]
fn lua_error_clears_frame_above_host_stack() {
    let mut state = State::new();

    state.push_number(7.0);
    state.push_string("host sentinel");
    let top_before = state.get_top();

    state
        .load_string(
            r#"
            local function explode(a, b)
                local values = a .. b
                local function capture_values()
                    return values
                end
                error("lua frame failed")
            end

            return explode("alpha", "beta")
        "#,
        )
        .unwrap();

    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("expected Lua error");
    assert!(format!("{err}").contains("lua frame failed"));

    assert_eq!(state.get_top(), top_before);
    assert_eq!(state.to_number(1).unwrap(), 7.0);
    assert_eq!(state.to_string(2).unwrap(), "host sentinel");

    state.load_string("return 99").unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.get_top(), top_before + 1);
    assert_eq!(state.to_number(-1).unwrap(), 99.0);
}

#[test]
fn non_callable_dynamic_call_error_clears_call_frame() {
    let mut state = State::new();

    state.push_string("host sentinel");
    let top_before = state.get_top();

    state
        .load_string(
            r#"
            local function many()
                return "c", "d"
            end

            local not_a_function = 17
            return not_a_function("a", "b", many())
        "#,
        )
        .unwrap();

    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    assert!(result.is_err(), "Expected non-callable value error");

    assert_eq!(state.get_top(), top_before);
    assert_eq!(state.to_string(1).unwrap(), "host sentinel");
}

/// Repeated callback errors should not accumulate stack-bottom drift.
#[test]
fn multiple_rustfn_errors() {
    let mut state = State::new();

    // Register error function
    state.push_rust_fn(|_state| {
        Err(Error::without_location(ErrorKind::InternalError(
            "error".to_string(),
        )))
    });
    state.set_global("error_fn");

    // Call it multiple times
    for i in 0..5 {
        state.push_number(i as f64);
        let top_before = state.get_top();

        state.get_global("error_fn");
        let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
        assert!(result.is_err(), "Expected error from rust fn");

        let top_after = state.get_top();

        // Each iteration should leave stack unchanged
        assert_eq!(top_before, top_after, "Stack corrupted on iteration {i}");
    }
}

#[test]
fn nested_dynamic_callback_error_clears_all_call_frames() {
    let mut state = State::new();

    state.push_rust_fn(|state| {
        assert_eq!(state.get_top(), 8);
        assert_eq!(state.to_string(1).unwrap(), "inner");
        assert_eq!(state.to_string(2).unwrap(), "m1");
        assert_eq!(state.to_string(3).unwrap(), "m2");
        assert_eq!(state.to_string(4).unwrap(), "outer");
        assert_eq!(state.to_string(5).unwrap(), "m1");
        assert_eq!(state.to_string(6).unwrap(), "m2");
        assert_eq!(state.to_string(7).unwrap(), "a");
        assert_eq!(state.to_string(8).unwrap(), "b");
        state.push_string("callback temporary");
        Err(Error::without_location(ErrorKind::InternalError(
            "nested dynamic callback failed".to_string(),
        )))
    });
    state.set_global("fail");

    state.push_string("host sentinel");
    let top_before = state.get_top();

    state
        .load_string(
            r#"
            local function many(...)
                return "m1", "m2", ...
            end

            local function inner(...)
                return fail("inner", many(...))
            end

            local function outer(...)
                return inner("outer", many(...))
            end

            return outer("a", "b")
        "#,
        )
        .unwrap();

    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("expected nested callback error");
    assert!(format!("{err}").contains("nested dynamic callback failed"));

    assert_eq!(state.get_top(), top_before);
    assert_eq!(state.to_string(1).unwrap(), "host sentinel");
}

#[test]
fn rustfn_called_inside_dynamic_arg_sees_only_its_arguments() {
    let mut state = State::new();

    state.push_rust_fn(|state| {
        let top = state.get_top() as f64;
        assert_eq!(state.to_string(1).unwrap(), "a");
        assert_eq!(state.to_string(2).unwrap(), "b");
        assert_eq!(state.to_string(3).unwrap(), "c");
        assert_eq!(state.typ(3), dellingr::LuaType::String);
        state.set_top(0);
        state.push_number(top);
        Ok(1)
    });
    state.set_global("arg_count");

    state
        .load_string(
            r#"
        local function test(name, condition)
            return condition
        end

        local a1,a2,a3,a4,a5,a6,a7,a8,a9,a10
        local a11,a12,a13,a14,a15,a16,a17,a18,a19,a20

        return test("dynamic arg", (function()
            return arg_count("a", "b", "c")
        end)())
    "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

    assert_eq!(state.to_number(-1).unwrap(), 3.0);
}

#[test]
fn rustfn_table_field_called_inside_dynamic_arg_sees_only_its_arguments() {
    let mut state = State::new();

    state.get_global("string");
    state.push_string("arg_probe");
    state.push_rust_fn(|state| {
        assert_eq!(state.get_top(), 3);
        assert_eq!(state.to_string(1).unwrap(), "hello");
        assert_eq!(state.to_string(2).unwrap(), "l");
        assert_eq!(state.to_string(3).unwrap(), "L");
        state.set_top(0);
        state.push_string("ok");
        Ok(1)
    });
    state.set_table_raw(-3).unwrap();
    state.pop(1);

    state
        .load_string(
            r#"
        local function test(name, condition)
            if not condition then
                error("Test failed: " .. name)
            end
        end

        local a1,a2,a3,a4,a5,a6,a7,a8,a9,a10,a11,a12,a13,a14,a15,a16,a17,a18,a19,a20

        test("arg probe", (function()
            return string.arg_probe("hello", "l", "L") == "ok"
        end)())
    "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
}
