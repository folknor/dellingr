//! Host error callback behavior at host initiated call boundaries.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use dellingr::error::{Error, ErrorKind};
use dellingr::{ArgCount, HostCallbacks, RetCount, State};

struct ErrorCounter(Arc<AtomicUsize>);

impl HostCallbacks for ErrorCounter {
    fn on_error(&mut self, _source: Option<&str>, _error: &Error) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn fail_fn(_state: &mut State) -> dellingr::Result<u8> {
    Err(Error::without_location(ErrorKind::InternalError(
        "host callback failed".into(),
    )))
}

fn run_setup(state: &mut State, source: &str) {
    state
        .load_string(source)
        .expect("the setup chunk should compile");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("the setup chunk should run");
}

fn call_global(state: &mut State, name: &str) -> Error {
    state
        .get_global(name)
        .expect("the callable global should exist");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("the callable should fail")
}

#[test]
fn host_call_of_cyclic_call_metamethod_notifies_once() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
    run_setup(
        &mut state,
        "callable = {}; setmetatable(callable, { __call = callable })",
    );

    let error = call_global(&mut state, "callable");

    assert!(matches!(
        error.kind,
        ErrorKind::MetamethodDepthExceeded { .. }
    ));
    assert!(error.stack_trace.is_empty());
    assert_eq!(state.get_top(), 0);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn host_call_through_callable_table_chain_notifies_once() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
    state
        .push_rust_fn(fail_fn)
        .expect("the Rust function should push");
    state.set_global("fail");
    run_setup(
        &mut state,
        r#"
            local inner = setmetatable({}, { __call = fail })
            callable = setmetatable({}, { __call = inner })
        "#,
    );

    let error = call_global(&mut state, "callable");

    assert!(matches!(error.kind, ErrorKind::InternalError(_)));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn host_call_through_chain_into_failing_lua_function_notifies_once() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
    run_setup(
        &mut state,
        r#"
            local inner = setmetatable({}, { __call = function() error("lua failed") end })
            callable = setmetatable({}, { __call = inner })
        "#,
    );

    let _error = call_global(&mut state, "callable");

    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn host_table_read_through_failing_rust_index_notifies_once() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
    state
        .push_rust_fn(fail_fn)
        .expect("the Rust function should push");
    state.set_global("fail");
    run_setup(&mut state, "indexed = setmetatable({}, { __index = fail })");

    state
        .get_global("indexed")
        .expect("the indexed global should exist");
    state.push_string("missing").expect("the key should push");
    let error = state
        .get_table(1)
        .expect_err("the __index handler should fail");

    assert!(matches!(error.kind, ErrorKind::InternalError(_)));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn host_call_through_direct_rust_call_metamethod_notifies_once() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
    state
        .push_rust_fn(fail_fn)
        .expect("the Rust function should push");
    state.set_global("fail");
    run_setup(&mut state, "callable = setmetatable({}, { __call = fail })");

    let error = call_global(&mut state, "callable");

    assert!(matches!(error.kind, ErrorKind::InternalError(_)));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
