//! Host error callback behavior at host initiated call boundaries.

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

fn fail_fn(_state: &mut State) -> dellingr::Result<u8> {
    Err(Error::without_location(ErrorKind::HostError(
        "host callback failed".into(),
    )))
}

fn run_setup(state: &mut State, source: &str) {
    dellingr::Program::compile(source, None)
        .and_then(|program| state.load(&program))
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
        error.kind(),
        ErrorKind::MetamethodDepthExceeded { .. }
    ));
    assert!(error.stack_trace().is_empty());
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
    state.set_global("fail").unwrap();
    run_setup(
        &mut state,
        r#"
            local inner = setmetatable({}, { __call = fail })
            callable = setmetatable({}, { __call = inner })
        "#,
    );

    let error = call_global(&mut state, "callable");

    assert!(matches!(error.kind(), ErrorKind::HostError(_)));
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
    state.set_global("fail").unwrap();
    run_setup(&mut state, "indexed = setmetatable({}, { __index = fail })");

    state
        .get_global("indexed")
        .expect("the indexed global should exist");
    state.push_string("missing").expect("the key should push");
    let error = state
        .get_table(1)
        .expect_err("the __index handler should fail");

    assert!(matches!(error.kind(), ErrorKind::HostError(_)));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn host_call_through_direct_rust_call_metamethod_notifies_once() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
    state
        .push_rust_fn(fail_fn)
        .expect("the Rust function should push");
    state.set_global("fail").unwrap();
    run_setup(&mut state, "callable = setmetatable({}, { __call = fail })");

    let error = call_global(&mut state, "callable");

    assert!(matches!(error.kind(), ErrorKind::HostError(_)));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn direct_non_callable_and_terminal_call_chain_notify_once() {
    for setup in [
        "callable = 42",
        "callable = setmetatable({}, { __call = 42 })",
        "local inner = setmetatable({}, { __call = 42 }); callable = setmetatable({}, { __call = inner })",
    ] {
        let count = Arc::new(AtomicUsize::new(0));
        let mut state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
        run_setup(&mut state, setup);
        let error = call_global(&mut state, "callable");
        assert!(matches!(error.kind(), ErrorKind::TypeError(_)));
        assert!(error.to_string().contains("attempt to call a number value"));
        assert_eq!(state.get_top(), 0);
        assert_eq!(count.load(Ordering::SeqCst), 1, "{setup}");
    }
}

fn call_non_callable(state: &mut State) -> dellingr::Result<u8> {
    state.push_number(42.0)?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))?;
    Ok(0)
}

#[test]
fn rust_callback_propagating_nested_call_failure_notifies_once() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
    state
        .push_rust_fn(call_non_callable)
        .expect("callback fits");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("the nested non-callable call fails");
    assert!(matches!(error.kind(), ErrorKind::TypeError(_)));
    assert_eq!(state.get_top(), 0);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn malformed_host_call_does_not_notify() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("a call without a function is malformed");
    assert!(matches!(error.kind(), ErrorKind::InvalidStackIndex { .. }));
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

fn call_failing_callback(state: &mut State) -> dellingr::Result<u8> {
    state.push_rust_fn(fail_fn)?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))?;
    Ok(0)
}

#[test]
fn rust_callback_propagating_nested_rust_failure_notifies_once() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
    state
        .push_rust_fn(call_failing_callback)
        .expect("callback fits");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("the nested Rust callback fails");
    assert!(matches!(error.kind(), ErrorKind::HostError(_)));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

fn fail_with_trace(_state: &mut State) -> dellingr::Result<u8> {
    Err(
        Error::without_location(ErrorKind::HostError("host failure".into())).with_stack_trace(
            vec![dellingr::error::StackFrame {
                function_name: Some("host".into()),
                source: Some(b"host-source".as_slice().into()),
                line: 7,
                defined_line: 0,
            }],
        ),
    )
}

#[test]
fn host_supplied_trace_does_not_suppress_notification() {
    for through_lua in [false, true] {
        let count = Arc::new(AtomicUsize::new(0));
        let mut state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
        state.push_rust_fn(fail_with_trace).expect("callback fits");
        if through_lua {
            state.set_global("fail").unwrap();
            dellingr::Program::compile("fail()", None)
                .and_then(|program| state.load(&program))
                .expect("chunk compiles");
        }
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("callback fails");
        assert_eq!(
            error.stack_trace()[0].source.as_deref(),
            Some(b"host-source".as_slice())
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}

fn return_nothing(_state: &mut State) -> dellingr::Result<u8> {
    Ok(0)
}

#[test]
fn execution_stack_preflights_notify_once() {
    for case in 0..4 {
        let count = Arc::new(AtomicUsize::new(0));
        let mut state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
        match case {
            0 => {
                run_setup(
                    &mut state,
                    "function target(a, b) local c = a; return c end",
                );
            }
            1 => {
                state.push_rust_fn(return_nothing).expect("callback fits");
                state.set_global("target").unwrap();
            }
            2 => {
                run_setup(&mut state, "function target() end");
            }
            _ => {
                run_setup(
                    &mut state,
                    "target = setmetatable({}, { __call = function() end })",
                );
            }
        }
        // Leave one slot for the callable. Removing it admits the call, but
        // frame setup / return padding / __call insertion must exceed the cap.
        for _ in 0..dellingr::limits::MAX_STACK_SIZE - 1 {
            state
                .push_nil()
                .expect("filler remains within the real limit");
        }
        state
            .get_global("target")
            .expect("callable fits at the cap");
        // Cases 1 and 2 return no values into a fixed two-result call, so the
        // caller-side nil padding is what exceeds the cap.
        let expected = if case == 1 || case == 2 {
            RetCount::Fixed(2)
        } else {
            RetCount::Fixed(0)
        };
        let error = state
            .call(ArgCount::Fixed(0), expected)
            .expect_err("preflight exceeds the real stack limit");
        assert!(matches!(error.kind(), ErrorKind::StackOverflow { .. }));
        assert_eq!(count.load(Ordering::SeqCst), 1, "case {case}");
        assert_eq!(state.get_top(), dellingr::limits::MAX_STACK_SIZE - 1);
        if case == 0 {
            assert!(!error.stack_trace().is_empty());
        }
    }
}

#[test]
fn callable_argument_overflow_notifies_once() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
    run_setup(
        &mut state,
        "target = setmetatable({}, { __call = function() end })",
    );
    state.get_global("target").expect("callable fits");
    for _ in 0..dellingr::limits::MAX_ARGUMENTS {
        state.push_nil().expect("arguments fit");
    }
    let error = state
        .call(
            ArgCount::Fixed(dellingr::limits::MAX_ARGUMENTS),
            RetCount::Fixed(0),
        )
        .expect_err("implicit self exceeds argument ABI");
    assert!(matches!(error.kind(), ErrorKind::TooManyArguments { .. }));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(state.get_top(), 0);
}

fn return_saved_error(state: &mut State) -> dellingr::Result<u8> {
    Err(state
        .user_data_mut::<Option<Error>>()
        .expect("saved error slot exists")
        .take()
        .expect("saved error is present"))
}

#[test]
fn returning_the_same_error_value_preserves_reporting_state() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
    state.push_rust_fn(fail_fn).expect("callback fits");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("first call fails");
    state.set_user_data(Some(error));
    state
        .push_rust_fn(return_saved_error)
        .expect("callback fits");
    let _error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("second call returns the same error");
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn nonfunction_chain_resolution_and_admission_report_once() {
    fn host_admit(state: &mut State) -> dellingr::Result<u8> {
        state.get_global("target")?;
        state.call(ArgCount::Fixed(0), RetCount::Fixed(0))?;
        Ok(0)
    }

    for from_lua in [false, true] {
        for admission in [false, true] {
            let count = Arc::new(AtomicUsize::new(0));
            let mut state = State::with_callbacks(Box::new(ErrorCounter(Arc::clone(&count))));
            let terminal = if admission { "function() end" } else { "5" };
            run_setup(
                &mut state,
                &format!(
                    "local inner = setmetatable({{}}, {{__call = {terminal}}})
                target = setmetatable({{}}, {{__call = inner}})"
                ),
            );
            if admission {
                state.push_rust_fn(host_admit).unwrap();
                state.set_global("host_admit").unwrap();
                // Fill Lua activation depth without holding chain units at
                // intervening levels; the final admission exercises either
                // call_from_lua or a nested call_unguarded from Rust.
                let source = format!(
                    "function recurse(n)
                    if n < {} then return recurse(n + 1) end
                    return {}()
                    end",
                    dellingr::limits::MAX_CALL_DEPTH - 1 - u32::from(from_lua),
                    if from_lua { "target" } else { "host_admit" }
                );
                let program = dellingr::Program::compile(source, Some(b"chain-admission")).unwrap();
                state.load(&program).unwrap();
                state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
            }
            let error = if from_lua {
                let source = if admission {
                    "return recurse(0)"
                } else {
                    "return target()"
                };
                let program = dellingr::Program::compile(source, Some(b"chain-caller")).unwrap();
                state.load(&program).unwrap();
                state
                    .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                    .expect_err("chain fails")
            } else {
                state
                    .get_global(if admission { "recurse" } else { "target" })
                    .unwrap();
                if admission {
                    state.push_number(0.0).unwrap();
                }
                state
                    .call(ArgCount::Fixed(u8::from(admission)), RetCount::Fixed(0))
                    .expect_err("chain fails")
            };
            if admission {
                assert!(
                    matches!(error.kind(), ErrorKind::CallDepthExceeded { .. }),
                    "{error:?}"
                );
                assert_eq!(error.line_num(), 3);
                assert_eq!(error.source_name(), Some(b"chain-admission".as_slice()));
                let trace = error.stack_trace();
                assert_eq!(trace.len(), dellingr::limits::MAX_CALL_DEPTH as usize);
                assert_eq!(trace[0].line, 3);
                assert_eq!(
                    trace[0].source.as_deref(),
                    Some(b"chain-admission".as_slice())
                );
                for frame in &trace[1..trace.len() - usize::from(from_lua)] {
                    assert_eq!(frame.line, 2);
                    assert_eq!(frame.source.as_deref(), Some(b"chain-admission".as_slice()));
                }
                if from_lua {
                    assert_eq!(trace.last().unwrap().line, 1);
                    assert_eq!(
                        trace.last().unwrap().source.as_deref(),
                        Some(b"chain-caller".as_slice())
                    );
                }
            } else {
                assert!(matches!(error.kind(), ErrorKind::TypeError(_)));
                if from_lua {
                    assert_eq!(error.line_num(), 1);
                    assert_eq!(error.source_name(), Some(b"chain-caller".as_slice()));
                    assert_eq!(error.stack_trace().len(), 1);
                    assert_eq!(error.stack_trace()[0].line, 1);
                    assert_eq!(
                        error.stack_trace()[0].source.as_deref(),
                        Some(b"chain-caller".as_slice())
                    );
                } else {
                    assert!(error.stack_trace().is_empty());
                }
            }
            assert_eq!(count.load(Ordering::SeqCst), 1);
            assert_eq!(state.get_top(), 0);
            run_setup(
                &mut state,
                &format!(
                    "local t = setmetatable({{}}, {{__call = function() return 7 end}})
                for i = 1, {} do t = setmetatable({{}}, {{__call = t}}) end
                if t() ~= 7 then error('reuse') end",
                    dellingr::limits::MAX_METAMETHOD_DEPTH
                ),
            );
            assert_eq!(count.load(Ordering::SeqCst), 1);
            #[cfg(feature = "snapshot")]
            {
                state.push_nil().unwrap();
                state.set_global("host_admit").unwrap();
                state.save_state().unwrap();
            }
        }
    }
}
