//! Test that demonstrates stack_bottom corruption when rust_fn returns error.

use dellingr::error::{Error, ErrorKind};
use dellingr::{ArgCount, RetCount, State};

/// This test demonstrates that stack_bottom is not restored when a rust_fn returns an error.
/// The bug: in vm/eval.rs call(), when RustFn returns Err, the ? operator propagates
/// the error but skips the restoration of stack_bottom = old_stack_bottom.
#[test]
fn rustfn_error_corrupts_stack_bottom() {
    let mut state = State::new();

    // Register a rust function that returns an error
    state.push_rust_fn(|_state| {
        Err(Error::without_location(ErrorKind::InternalError(
            "intentional error".to_string(),
        )))
    });
    state.set_global("error_fn");

    // Push some values to establish a stack state
    state.push_number(1.0);
    state.push_number(2.0);

    let top_before = state.get_top();
    println!("Stack top before call: {}", top_before);
    assert_eq!(top_before, 2, "Should have 2 values on stack");

    // Call the error function - this should fail
    state.get_global("error_fn");
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));

    assert!(result.is_err(), "Expected error from rust fn");
    println!("Got expected error: {:?}", result.unwrap_err());

    let top_after = state.get_top();
    println!("Stack top after error: {}", top_after);

    // THE BUG: stack_bottom is corrupted, so get_top() returns wrong value
    // We pushed 2 values before the call, so top should still be 2.
    // But because stack_bottom wasn't restored, get_top() = stack.len() - corrupted_stack_bottom
    // This might return a huge number (unsigned underflow) or a wrong small number.
    assert_eq!(
        top_before, top_after,
        "Stack top changed after error! Before: {}, After: {}. stack_bottom likely corrupted.",
        top_before, top_after
    );
}

/// Test that stack operations work correctly after a rust_fn error.
#[test]
fn stack_operations_after_rustfn_error() {
    let mut state = State::new();

    // Register a rust function that returns an error
    state.push_rust_fn(|_state| {
        Err(Error::without_location(ErrorKind::InternalError(
            "intentional error".to_string(),
        )))
    });
    state.set_global("error_fn");

    // Push a known value
    state.push_number(42.0);

    // Call the error function
    state.get_global("error_fn");
    let _ = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));

    // Try to read the value we pushed - should be 42.0
    // If stack_bottom is corrupted, this will either:
    // - Return wrong value
    // - Panic (debug) / segfault (release) due to bad index
    let val = state.to_number(-1).expect("Stack value should be readable");
    println!("Value at top of stack: {:?}", val);
    assert_eq!(val, 42.0, "Stack value should still be 42.0");
}

/// Test multiple error calls to see if corruption accumulates
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
        let _ = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));

        let top_after = state.get_top();
        println!(
            "Iteration {}: top before={}, after={}",
            i, top_before, top_after
        );

        // Each iteration should leave stack unchanged
        assert_eq!(top_before, top_after, "Stack corrupted on iteration {}", i);
    }
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
    state.push_rust_fn(|state| {
        assert_eq!(state.get_top(), 3);
        assert_eq!(state.to_string(1).unwrap(), "hello");
        assert_eq!(state.to_string(2).unwrap(), "l");
        assert_eq!(state.to_string(3).unwrap(), "L");
        state.set_top(0);
        state.push_string("ok".to_string());
        Ok(1)
    });
    state.push_string("arg_probe".to_string());
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
