//! `on_error` contract at the host call entry points: misuse rejected before
//! dispatch is returned only; failures after dispatch notify exactly once.

use std::sync::{Arc, Mutex};

use dellingr::error::{Error, ErrorKind};
use dellingr::{ArgCount, HostCallbacks, RetCount, State};

#[derive(Clone)]
struct ErrorCounter(Arc<Mutex<usize>>);

impl HostCallbacks for ErrorCounter {
    fn on_print(&mut self, _source: Option<&str>, _line: u32, _message: &str) {}

    fn on_error(&mut self, _error: &Error) {
        *self.0.lock().expect("error counter mutex") += 1;
    }
}

fn counted_state() -> (State, Arc<Mutex<usize>>) {
    let counter = Arc::new(Mutex::new(0));
    let state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&counter))));
    (state, counter)
}

fn count(counter: &Arc<Mutex<usize>>) -> usize {
    *counter.lock().expect("error counter mutex")
}

#[test]
fn call_preflight_misuse_does_not_notify() {
    let (mut state, counter) = counted_state();
    let err = state
        .call(ArgCount::Fixed(3), RetCount::Fixed(0))
        .expect_err("too few stack values");
    assert!(matches!(err.kind(), ErrorKind::InvalidStackIndex { .. }));
    state.load_string("return 1").unwrap();
    state
        .call(ArgCount::Dynamic, RetCount::Fixed(0))
        .expect_err("Dynamic is bytecode-only");
    assert_eq!(
        state.get_top(),
        1,
        "rejected call leaves the stack unchanged"
    );
    assert_eq!(count(&counter), 0);
}

#[test]
fn call_anchor_preflight_misuse_does_not_notify() {
    let (mut state, counter) = counted_state();
    state.load_string("return 1").unwrap();
    let anchor = state.anchor_function().unwrap();

    let err = state
        .call_anchor(anchor, ArgCount::Fixed(2), RetCount::Fixed(0))
        .expect_err("too few stack values");
    assert!(matches!(err.kind(), ErrorKind::InvalidStackIndex { .. }));
    state
        .call_anchor(anchor, ArgCount::Dynamic, RetCount::Fixed(0))
        .expect_err("Dynamic is bytecode-only");

    assert!(state.release_anchor(anchor));
    let err = state
        .call_anchor(anchor, ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("released anchor");
    assert!(matches!(err.kind(), ErrorKind::InvalidAnchor));

    assert_eq!(state.get_top(), 0);
    assert_eq!(count(&counter), 0);
}

#[test]
fn call_anchor_dispatch_error_notifies_once() {
    let (mut state, counter) = counted_state();
    state.load_string("error('boom')").unwrap();
    let anchor = state.anchor_function().unwrap();
    state
        .call_anchor(anchor, ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("script raises");
    assert_eq!(count(&counter), 1);
}

#[test]
fn call_anchor_without_room_for_callee_does_not_notify() {
    let (mut state, counter) = counted_state();
    state.load_string("return 1").unwrap();
    let anchor = state.anchor_function().unwrap();
    let cap = dellingr::limits::MAX_STACK_SIZE;
    state
        .set_top(isize::try_from(cap).expect("stack cap fits in isize"))
        .expect("filler fits up to the cap");
    let err = state
        .call_anchor(anchor, ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("inserting the callee would exceed the cap");
    assert!(matches!(err.kind(), ErrorKind::StackOverflow { .. }));
    assert_eq!(state.get_top(), cap);
    assert_eq!(state.anchor_type(anchor), Some(dellingr::LuaType::Function));
    assert_eq!(count(&counter), 0);
}
