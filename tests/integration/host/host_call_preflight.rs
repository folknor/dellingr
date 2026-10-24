//! `on_error` contract at the host call entry points: misuse rejected before
//! dispatch is returned only; failures after dispatch notify exactly once.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

use crate::support::PushNewRustFn;
use crate::support::{self, Recorder};

#[test]
fn call_preflight_misuse_does_not_notify() {
    let (mut state, recorder) = Recorder::state();
    let err = state
        .call(ArgCount::Fixed(3), RetCount::Fixed(0))
        .expect_err("too few stack values");
    assert!(matches!(err.kind(), ErrorKind::InvalidStackIndex { .. }));
    support::load(&mut state, "return 1");
    state
        .call(ArgCount::Dynamic, RetCount::Fixed(0))
        .expect_err("Dynamic is bytecode-only");
    assert_eq!(
        state.get_top(),
        1,
        "rejected call leaves the stack unchanged"
    );
    assert_eq!(recorder.error_count(), 0);
}

#[test]
fn call_anchor_preflight_misuse_does_not_notify() {
    let (mut state, recorder) = Recorder::state();
    support::load(&mut state, "return 1");
    let anchor = state.anchor_function().unwrap();

    let err = state
        .call_anchor(&anchor, ArgCount::Fixed(2), RetCount::Fixed(0))
        .expect_err("too few stack values");
    assert!(matches!(err.kind(), ErrorKind::InvalidStackIndex { .. }));
    state
        .call_anchor(&anchor, ArgCount::Dynamic, RetCount::Fixed(0))
        .expect_err("Dynamic is bytecode-only");

    let mut other = State::new();
    support::load(&mut other, "return 1");
    let foreign = other.anchor_function().unwrap();
    let err = state
        .call_anchor(&foreign, ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("wrong-State anchor");
    assert!(matches!(err.kind(), ErrorKind::InvalidAnchor));

    assert_eq!(state.get_top(), 0);
    assert_eq!(recorder.error_count(), 0);
}

#[test]
fn call_anchor_dispatch_error_notifies_once() {
    let (mut state, recorder) = Recorder::state();
    support::load(&mut state, "error('boom')");
    let anchor = state.anchor_function().unwrap();
    state
        .call_anchor(&anchor, ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("script raises");
    assert_eq!(recorder.error_count(), 1);
}

#[test]
fn call_anchor_without_room_for_callee_does_not_notify() {
    let (mut state, recorder) = Recorder::state();
    support::load(&mut state, "return 1");
    let anchor = state.anchor_function().unwrap();
    let cap = dellingr::limits::MAX_STACK_SIZE;
    state
        .set_top(isize::try_from(cap).expect("stack cap fits in isize"))
        .expect("filler fits up to the cap");
    let err = state
        .call_anchor(&anchor, ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("inserting the callee would exceed the cap");
    assert!(matches!(err.kind(), ErrorKind::StackOverflow { .. }));
    assert_eq!(state.get_top(), cap);
    assert_eq!(
        state.anchor_type(&anchor),
        Some(dellingr::LuaType::Function)
    );
    assert_eq!(recorder.error_count(), 0);
}

#[test]
fn rust_call_releases_callee_slot_before_callback_push() {
    let mut state = State::new();
    let cap = dellingr::limits::MAX_STACK_SIZE;
    state.set_top((cap - 2) as isize).unwrap();
    state
        .push_new_rust_fn(|state| {
            assert_eq!(state.to_number(1)?, 42.0);
            state.push_number(23.0)?;
            Ok(1)
        })
        .unwrap();
    state.push_number(42.0).unwrap();
    // The stack is full; removing the callee frees the callback's result slot.
    state.call(ArgCount::Fixed(1), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 23.0);
}
