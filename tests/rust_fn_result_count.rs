//! A Rust callback's reported result count is validated against its frame.
//! Over-reporting is a host bug and fails the call instead of being padded
//! with nil; every RustFn call path shares that check.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use dellingr::error::{Error, ErrorKind};
use dellingr::{ArgCount, HostCallbacks, RetCount, State};

struct ErrorCounter(Arc<AtomicUsize>);

impl HostCallbacks for ErrorCounter {
    fn on_print(&mut self, _source: Option<&[u8]>, _line: u32, _message: &[u8]) {}

    fn on_error(&mut self, _error: &Error) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Pushes one value and claims three.
fn over_report(state: &mut State) -> dellingr::Result<u8> {
    state.push_number(7.0)?;
    Ok(3)
}

/// Generic-for iterator: pushes one value onto its two arguments, claims four.
fn over_report_iterator(state: &mut State) -> dellingr::Result<u8> {
    state.push_number(7.0)?;
    Ok(4)
}

/// Pushes nothing and returns its two arguments.
fn return_args(_state: &mut State) -> dellingr::Result<u8> {
    Ok(2)
}

/// Pushes three values and returns only the top two.
fn return_top_two(state: &mut State) -> dellingr::Result<u8> {
    state.push_number(1.0)?;
    state.push_number(2.0)?;
    state.push_number(3.0)?;
    Ok(2)
}

/// Calls `over_report` and checks the failure left this callback's frame
/// intact before propagating the error.
fn nested_over_report(state: &mut State) -> dellingr::Result<u8> {
    state.push_number(42.0)?;
    state.push_rust_fn(over_report)?;
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect_err("inner callback over-reports");
    if state.get_top() != 1 || state.to_number(1)? != 42.0 {
        return Err(Error::without_location(ErrorKind::HostError(
            "outer frame disturbed".into(),
        )));
    }
    Err(error)
}

fn counting_state() -> (State, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
    (state, count)
}

fn assert_over_report(kind: &ErrorKind) {
    assert!(
        matches!(
            kind,
            ErrorKind::ResultCountExceedsStack {
                reported: 3,
                available: 1
            }
        ),
        "unexpected error kind: {kind:?}"
    );
}

#[test]
fn direct_host_call_rejects_over_report() {
    let (mut state, count) = counting_state();
    state.push_string("below").unwrap();
    state.push_rust_fn(over_report).unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::All)
        .expect_err("over-report fails");
    assert_over_report(error.kind());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(state.get_top(), 1, "the call frame is cleared");
}

#[test]
fn lua_call_rejects_over_report() {
    let (mut state, count) = counting_state();
    state.push_rust_fn(over_report).unwrap();
    state.set_global("bad").unwrap();
    dellingr::Program::compile("local a, b, c = bad()", None)
        .and_then(|program| state.load(&program))
        .unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("over-report fails");
    assert_over_report(error.kind());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(!error.stack_trace().is_empty());
}

#[test]
fn generic_for_iterator_rejects_over_report() {
    let (mut state, count) = counting_state();
    state.push_rust_fn(over_report_iterator).unwrap();
    state.set_global("bad").unwrap();
    dellingr::Program::compile("for k, v in bad do error('unreachable') end", None)
        .and_then(|program| state.load(&program))
        .unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("over-report fails");
    // The iterator receives (state, control) = (nil, nil), so its frame holds
    // the two arguments plus its one push.
    // Reporting 3 is therefore valid; the test iterator below reports 4.
    assert!(
        matches!(
            error.kind(),
            ErrorKind::ResultCountExceedsStack {
                reported: 4,
                available: 3
            }
        ),
        "unexpected error kind: {:?}",
        error.kind()
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn nested_over_report_restores_outer_frame_and_reports_once() {
    let (mut state, count) = counting_state();
    state.push_rust_fn(nested_over_report).unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("inner failure propagates");
    assert_over_report(error.kind());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(state.get_top(), 0);
}

#[test]
fn arguments_count_as_results() {
    let (mut state, _count) = counting_state();
    state.push_rust_fn(return_args).unwrap();
    state.push_number(1.0).unwrap();
    state.push_number(2.0).unwrap();
    state.call(ArgCount::Fixed(2), RetCount::All).unwrap();
    assert_eq!(state.get_top(), 2);
    assert_eq!(state.to_number(1).unwrap(), 1.0);
    assert_eq!(state.to_number(2).unwrap(), 2.0);
}

#[test]
fn under_report_keeps_top_values_in_order() {
    let (mut state, _count) = counting_state();
    state.push_rust_fn(return_top_two).unwrap();
    state.push_number(9.0).unwrap();
    state.call(ArgCount::Fixed(1), RetCount::All).unwrap();
    assert_eq!(state.get_top(), 2);
    assert_eq!(state.to_number(1).unwrap(), 2.0);
    assert_eq!(state.to_number(2).unwrap(), 3.0);
}

#[test]
fn caller_side_padding_is_unchanged() {
    let (mut state, _count) = counting_state();
    state.push_rust_fn(return_top_two).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(4)).unwrap();
    assert_eq!(state.get_top(), 4);
    assert_eq!(state.to_number(1).unwrap(), 2.0);
    assert_eq!(state.to_number(2).unwrap(), 3.0);
    assert_eq!(state.typ(3), dellingr::LuaType::Nil);
    assert_eq!(state.typ(4), dellingr::LuaType::Nil);
}
