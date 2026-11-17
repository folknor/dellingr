//! `State::invoke`: typed host calls into an anchored function, which own
//! their stack cleanup on every ordinary exit.

use dellingr::error::{ArgDetail, ErrorKind};
use dellingr::{Anchor, ArgCount, LuaType, RetCount, State};

use crate::support::PushNewRustFn;
use crate::support::{self, Recorder};

/// Runs `source` and anchors its global `name`.
fn define(state: &mut State, source: &str, name: &str) -> Anchor {
    support::load(state, source);
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("setup chunk runs");
    state.get_global(name).expect("global read");
    state.anchor_function().expect("a function global")
}

#[test]
fn arguments_and_results_round_trip_and_leave_the_stack_alone() {
    let mut state = State::new();
    let f = define(
        &mut state,
        "function f(a, b, c, d) return a + b, c, d, 'text' end",
        "f",
    );
    state.push_number(7.0).unwrap();
    let (sum, flag, n, text): (f64, bool, i64, String) =
        state.invoke(&f, (1.5_f64, 2_i32, true, 9_u32)).unwrap();
    assert_eq!((sum, flag, n, text.as_str()), (3.5, true, 9, "text"));
    assert_eq!(state.get_top(), 1, "the host's own value is untouched");
    assert_eq!(state.to_number(1).unwrap(), 7.0);
    let () = state.invoke(&f, (1.0_f64, 1.0_f64)).unwrap();
    assert_eq!(state.get_top(), 1);
}

#[test]
fn missing_results_are_nil_and_extra_ones_are_dropped() {
    let mut state = State::new();
    let f = define(&mut state, "function f() return 1, 2, 3 end", "f");
    let (a, b): (u8, Option<f64>) = state.invoke(&f, ()).unwrap();
    assert_eq!((a, b), (1, Some(2.0)));
    let g = define(&mut state, "function g() end", "g");
    let (a, b): (Option<f64>, Option<String>) = state.invoke(&g, ()).unwrap();
    assert_eq!((a, b), (None, None));
    assert_eq!(state.get_top(), 0);
}

#[test]
fn a_nan_result_leaves_as_the_canonical_nan() {
    let mut state = State::new();
    let f = define(&mut state, "function f() return 0/0 end", "f");
    let (n,): (f64,) = state.invoke(&f, ()).unwrap();
    assert_eq!(n.to_bits(), 0x7ff8_0000_0000_0000);
}

#[test]
fn a_conversion_failure_names_the_result_keeps_the_effects_and_does_not_notify() {
    let (mut state, recorder) = Recorder::state();
    let f = define(
        &mut state,
        "calls = 0 function f() calls = calls + 1 return 1, 'x' end",
        "f",
    );
    let err = state
        .invoke::<_, (f64, f64)>(&f, ())
        .expect_err("a string is not a number");
    match err.kind() {
        ErrorKind::ResultConversion {
            index,
            detail,
            expected,
            received,
        } => {
            assert_eq!(*index, 2);
            assert_eq!(*detail, ArgDetail::TypeMismatch);
            assert_eq!(*expected, LuaType::Number);
            assert_eq!(*received, LuaType::String);
        }
        other => panic!("unexpected error kind {other:?}"),
    }
    assert_eq!(err.to_string(), "result #2: number expected, got string");
    let h = define(&mut state, "function h() return 1.5 end", "h");
    let err = state
        .invoke::<_, (u8,)>(&h, ())
        .expect_err("not an integer");
    assert_eq!(
        err.to_string(),
        "result #1: number has no integer representation"
    );
    assert_eq!(state.get_top(), 0, "the results are dropped");
    state.get_global("calls").unwrap();
    assert_eq!(
        state.to_number(-1).unwrap(),
        1.0,
        "the call's effects stand"
    );
    assert_eq!(recorder.error_count(), 0);
}

#[test]
fn a_call_error_restores_the_stack_and_notifies_once() {
    let (mut state, recorder) = Recorder::state();
    let f = define(&mut state, "function f(x) error('boom') end", "f");
    state.push_boolean(true).unwrap();
    let err = state
        .invoke::<_, (f64,)>(&f, (1.0_f64,))
        .expect_err("the callee raises");
    assert!(matches!(err.kind(), ErrorKind::ScriptError { .. }));
    assert_eq!(state.get_top(), 1);
    assert_eq!(recorder.error_count(), 1);
}

#[test]
fn a_foreign_anchor_is_refused_before_anything_runs() {
    let (mut state, recorder) = Recorder::state();
    let mut other = State::new();
    let foreign = define(&mut other, "function f() end", "f");
    let err = state
        .invoke::<_, ()>(&foreign, (1.0_f64,))
        .expect_err("another State's handle");
    assert!(matches!(err.kind(), ErrorKind::InvalidAnchor));
    assert_eq!(state.get_top(), 0);
    assert_eq!(recorder.error_count(), 0);
}

/// `twice(f)` calls `f(21)` through `invoke` from inside a callback and
/// returns the doubled result.
fn twice(state: &mut State) -> dellingr::Result<u8> {
    let f = state.anchor_function_at(1)?;
    let result: dellingr::Result<(f64,)> = state.invoke(&f, (21_i32,));
    state.release_anchor(f);
    let (n,) = result?;
    state.push_number(n * 2.0)?;
    Ok(1)
}

#[test]
fn a_callback_invokes_relative_to_its_own_frame() {
    let mut state = State::new();
    state.push_new_rust_fn(twice).unwrap();
    state.set_global("twice").unwrap();
    support::load(
        &mut state,
        "local r = twice(function(x) return x end)
         if r ~= 42 then error('got ' .. tostring(r)) end",
    );
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
    assert_eq!(state.get_top(), 0);
}
