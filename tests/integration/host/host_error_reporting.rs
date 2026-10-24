//! Host error callback behavior at host initiated call boundaries.

use dellingr::error::{Error, ErrorKind, LibraryError};
use dellingr::{ArgCount, RetCount, State};

use crate::support::{self, Recorder, stack};

fn fail_fn(_state: &mut State) -> dellingr::Result<u8> {
    Err(Error::without_location(ErrorKind::HostError(
        "host callback failed".into(),
    )))
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
    let (mut state, recorder) = Recorder::state();
    support::run(
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
    assert_eq!(recorder.error_count(), 1);
}

#[test]
fn host_call_through_callable_table_chain_notifies_once() {
    let (mut state, recorder) = Recorder::state();
    state
        .push_rust_fn(fail_fn)
        .expect("the Rust function should push");
    state.set_global("fail").unwrap();
    support::run(
        &mut state,
        r#"
            local inner = setmetatable({}, { __call = fail })
            callable = setmetatable({}, { __call = inner })
        "#,
    );

    let error = call_global(&mut state, "callable");

    assert!(matches!(error.kind(), ErrorKind::HostError(_)));
    assert_eq!(recorder.error_count(), 1);
}

#[test]
fn host_call_through_chain_into_failing_lua_function_notifies_once() {
    let (mut state, recorder) = Recorder::state();
    support::run(
        &mut state,
        r#"
            local inner = setmetatable({}, { __call = function() error("lua failed") end })
            callable = setmetatable({}, { __call = inner })
        "#,
    );

    let _error = call_global(&mut state, "callable");

    assert_eq!(recorder.error_count(), 1);
}

#[test]
fn host_table_read_through_failing_rust_index_notifies_once() {
    let (mut state, recorder) = Recorder::state();
    state
        .push_rust_fn(fail_fn)
        .expect("the Rust function should push");
    state.set_global("fail").unwrap();
    support::run(&mut state, "indexed = setmetatable({}, { __index = fail })");

    state
        .get_global("indexed")
        .expect("the indexed global should exist");
    state.push_string("missing").expect("the key should push");
    let error = state
        .get_table(1)
        .expect_err("the __index handler should fail");

    assert!(matches!(error.kind(), ErrorKind::HostError(_)));
    assert_eq!(recorder.error_count(), 1);
}

#[test]
fn host_call_through_direct_rust_call_metamethod_notifies_once() {
    let (mut state, recorder) = Recorder::state();
    state
        .push_rust_fn(fail_fn)
        .expect("the Rust function should push");
    state.set_global("fail").unwrap();
    support::run(&mut state, "callable = setmetatable({}, { __call = fail })");

    let error = call_global(&mut state, "callable");

    assert!(matches!(error.kind(), ErrorKind::HostError(_)));
    assert_eq!(recorder.error_count(), 1);
}

#[test]
fn direct_non_callable_and_terminal_call_chain_notify_once() {
    for setup in [
        "callable = 42",
        "callable = setmetatable({}, { __call = 42 })",
        "local inner = setmetatable({}, { __call = 42 }); callable = setmetatable({}, { __call = inner })",
    ] {
        let (mut state, recorder) = Recorder::state();
        support::run(&mut state, setup);
        let error = call_global(&mut state, "callable");
        assert!(matches!(error.kind(), ErrorKind::TypeError(_)));
        assert!(error.to_string().contains("attempt to call a number value"));
        assert_eq!(state.get_top(), 0);
        assert_eq!(recorder.error_count(), 1, "{setup}");
    }
}

fn call_non_callable(state: &mut State) -> dellingr::Result<u8> {
    state.push_number(42.0)?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))?;
    Ok(0)
}

#[test]
fn rust_callback_propagating_nested_call_failure_notifies_once() {
    let (mut state, recorder) = Recorder::state();
    state
        .push_rust_fn(call_non_callable)
        .expect("callback fits");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("the nested non-callable call fails");
    assert!(matches!(error.kind(), ErrorKind::TypeError(_)));
    assert_eq!(state.get_top(), 0);
    assert_eq!(recorder.error_count(), 1);
}

#[test]
fn malformed_host_call_does_not_notify() {
    let (mut state, recorder) = Recorder::state();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("a call without a function is malformed");
    assert!(matches!(error.kind(), ErrorKind::InvalidStackIndex { .. }));
    assert_eq!(recorder.error_count(), 0);
}

fn call_failing_callback(state: &mut State) -> dellingr::Result<u8> {
    state.push_rust_fn(fail_fn)?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))?;
    Ok(0)
}

#[test]
fn rust_callback_propagating_nested_rust_failure_notifies_once() {
    let (mut state, recorder) = Recorder::state();
    state
        .push_rust_fn(call_failing_callback)
        .expect("callback fits");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("the nested Rust callback fails");
    assert!(matches!(error.kind(), ErrorKind::HostError(_)));
    assert_eq!(recorder.error_count(), 1);
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
        let (mut state, recorder) = Recorder::state();
        state.push_rust_fn(fail_with_trace).expect("callback fits");
        if through_lua {
            state.set_global("fail").unwrap();
            support::load(&mut state, "fail()");
        }
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("callback fails");
        assert_eq!(
            error.stack_trace()[0].source.as_deref(),
            Some(b"host-source".as_slice())
        );
        assert_eq!(recorder.error_count(), 1);
    }
}

fn return_nothing(_state: &mut State) -> dellingr::Result<u8> {
    Ok(0)
}

#[test]
fn execution_stack_preflights_notify_once() {
    for case in 0..4 {
        let (mut state, recorder) = Recorder::state();
        match case {
            0 => {
                support::run(
                    &mut state,
                    "function target(a, b) local c = a; return c end",
                );
            }
            1 => {
                state.push_rust_fn(return_nothing).expect("callback fits");
                state.set_global("target").unwrap();
            }
            2 => {
                support::run(&mut state, "function target() end");
            }
            _ => {
                support::run(
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
        assert_eq!(recorder.error_count(), 1, "case {case}");
        assert_eq!(state.get_top(), dellingr::limits::MAX_STACK_SIZE - 1);
        if case == 0 {
            // The refused callee never entered, and a host call has no
            // calling Lua frame, so no traceback names the callee.
            assert!(error.stack_trace().is_empty());
        }
    }
}

#[test]
fn callable_argument_overflow_notifies_once() {
    let (mut state, recorder) = Recorder::state();
    support::run(
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
    assert_eq!(recorder.error_count(), 1);
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
    let (mut state, recorder) = Recorder::state();
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
    assert_eq!(recorder.error_count(), 1);
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
            let (mut state, recorder) = Recorder::state();
            let terminal = if admission { "function() end" } else { "5" };
            support::run(
                &mut state,
                format!(
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
            assert_eq!(recorder.error_count(), 1);
            assert_eq!(state.get_top(), 0);
            support::run(
                &mut state,
                format!(
                    "local t = setmetatable({{}}, {{__call = function() return 7 end}})
                for i = 1, {} do t = setmetatable({{}}, {{__call = t}}) end
                if t() ~= 7 then error('reuse') end",
                    dellingr::limits::MAX_METAMETHOD_DEPTH
                ),
            );
            assert_eq!(recorder.error_count(), 1);
            #[cfg(feature = "snapshot")]
            {
                state.push_nil().unwrap();
                state.set_global("host_admit").unwrap();
                state.save_state().unwrap();
            }
        }
    }
}

/// One failing host operation, run either by the test or by a callback.
type Attempt = fn(&mut State) -> dellingr::Result<()>;

/// What the swallowing callback runs, and the errors it handled.
struct Harness {
    attempt: Attempt,
    swallowed: Vec<Error>,
}

fn harness(state: &mut State) -> dellingr::Result<&mut Harness> {
    state
        .user_data_mut::<Harness>()
        .ok_or_else(|| Error::without_location(ErrorKind::HostError("no harness".into())))
}

/// A callback that runs the harness attempt and handles its error instead of
/// returning it: it restores its frame and carries on successfully.
fn swallow(state: &mut State) -> dellingr::Result<u8> {
    let attempt = harness(state)?.attempt;
    let top = state.get_top() as isize;
    if let Err(error) = attempt(state) {
        state.set_top(top)?;
        harness(state)?.swallowed.push(error);
    }
    Ok(0)
}

const HARNESS_SETUP: &str = r#"
    cyclic = {}; setmetatable(cyclic, { __call = cyclic })
    callable = setmetatable({}, { __call = function() end })
    function lua_fail() error("lua failed") end
    index_false = setmetatable({}, { __index = false })
    index_cycle = {}; setmetatable(index_cycle, { __index = index_cycle })
    index_rust = setmetatable({}, { __index = fail })
    renders_table = setmetatable({}, { __tostring = function() return {} end })
    renders_fail = setmetatable({}, { __tostring = fail })
"#;

fn attempt_non_callable(state: &mut State) -> dellingr::Result<()> {
    state.push_number(42.0)?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
}

fn attempt_cyclic_chain(state: &mut State) -> dellingr::Result<()> {
    state.get_global("cyclic")?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
}

fn attempt_chain_arguments(state: &mut State) -> dellingr::Result<()> {
    state.get_global("callable")?;
    for _ in 0..dellingr::limits::MAX_ARGUMENTS {
        state.push_nil()?;
    }
    state.call(
        ArgCount::Fixed(dellingr::limits::MAX_ARGUMENTS),
        RetCount::Fixed(0),
    )
}

fn attempt_rust_error(state: &mut State) -> dellingr::Result<()> {
    state.push_rust_fn(fail_fn)?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
}

/// Fill the stack to one slot short of the cap and call a callback that
/// returns nothing into two fixed results: the nil padding cannot fit.
fn attempt_result_padding(state: &mut State) -> dellingr::Result<()> {
    while state.push_nil().is_ok() {}
    state.pop(1)?;
    state.push_rust_fn(return_nothing)?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(2))
}

fn attempt_lua_error(state: &mut State) -> dellingr::Result<()> {
    state.get_global("lua_fail")?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
}

fn attempt_index(state: &mut State, receiver: &str) -> dellingr::Result<()> {
    state.get_global(receiver)?;
    state.push_string("key")?;
    state.get_table(-2)
}

fn attempt_index_number(state: &mut State) -> dellingr::Result<()> {
    state.push_number(1.0)?;
    state.push_string("key")?;
    state.get_table(-2)
}

fn attempt_index_false(state: &mut State) -> dellingr::Result<()> {
    attempt_index(state, "index_false")
}

fn attempt_index_cycle(state: &mut State) -> dellingr::Result<()> {
    attempt_index(state, "index_cycle")
}

fn attempt_index_rust(state: &mut State) -> dellingr::Result<()> {
    attempt_index(state, "index_rust")
}

fn attempt_render_table(state: &mut State) -> dellingr::Result<()> {
    state.get_global("renders_table")?;
    state.tostring_bytes(-1).map(drop)
}

fn attempt_render_fail(state: &mut State) -> dellingr::Result<()> {
    state.get_global("renders_fail")?;
    state.tostring_bytes(-1).map(drop)
}

fn attempt_get_table_bad_index(state: &mut State) -> dellingr::Result<()> {
    state.push_string("key")?;
    state.get_table(5)
}

fn attempt_get_table_missing_key(state: &mut State) -> dellingr::Result<()> {
    state.new_table()?;
    state.get_table(-1)
}

fn attempt_tostring_bad_index(state: &mut State) -> dellingr::Result<()> {
    state.tostring_bytes(7).map(drop)
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    /// The test itself, as the outermost host, makes the failing call.
    Host,
    /// The host calls a callback that makes the call and handles the error.
    Callback,
    /// A Lua chunk calls that callback.
    FromLua,
}

/// Run `attempt` in `mode`; return the failure and the notifications.
fn run_attempt(attempt: Attempt, mode: Mode) -> (Error, Vec<String>) {
    let (mut state, recorder) = Recorder::state();
    state.push_rust_fn(fail_fn).unwrap();
    state.set_global("fail").unwrap();
    state.push_rust_fn(swallow).unwrap();
    state.set_global("swallow").unwrap();
    dellingr::Program::compile(HARNESS_SETUP, Some(b"setup"))
        .and_then(|program| state.load(&program))
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
    state.set_user_data(Harness {
        attempt,
        swallowed: Vec::new(),
    });
    let error = match mode {
        Mode::Host => {
            let error = attempt(&mut state).expect_err("the attempt fails");
            state.set_top(0).unwrap();
            error
        }
        Mode::Callback | Mode::FromLua => {
            if let Mode::FromLua = mode {
                dellingr::Program::compile("local before = 1\nswallow()", Some(b"swallow-caller"))
                    .and_then(|program| state.load(&program))
                    .unwrap();
            } else {
                state.push_rust_fn(swallow).unwrap();
            }
            state
                .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                .expect("the callback handles the error and succeeds");
            let mut swallowed =
                std::mem::take(&mut state.user_data_mut::<Harness>().unwrap().swallowed);
            assert_eq!(swallowed.len(), 1, "{mode:?}");
            swallowed.remove(0)
        }
    };
    assert_eq!(state.get_top(), 0, "{mode:?}: {error}");
    let notifications = recorder.errors();
    (error, notifications)
}

#[test]
fn execution_errors_notify_once_whether_returned_or_handled() {
    type Case = (&'static str, Attempt, fn(&ErrorKind) -> bool);
    let cases: [Case; 12] = [
        ("non_callable", attempt_non_callable, |kind| {
            matches!(kind, ErrorKind::TypeError(_))
        }),
        ("cyclic_chain", attempt_cyclic_chain, |kind| {
            matches!(kind, ErrorKind::MetamethodDepthExceeded { .. })
        }),
        ("chain_arguments", attempt_chain_arguments, |kind| {
            matches!(kind, ErrorKind::TooManyArguments { .. })
        }),
        ("rust_error", attempt_rust_error, |kind| {
            matches!(kind, ErrorKind::HostError(_))
        }),
        ("result_padding", attempt_result_padding, |kind| {
            matches!(kind, ErrorKind::StackOverflow { .. })
        }),
        ("lua_error", attempt_lua_error, |kind| {
            matches!(kind, ErrorKind::ScriptError { .. })
        }),
        ("index_number", attempt_index_number, |kind| {
            matches!(kind, ErrorKind::TypeError(_))
        }),
        ("index_false", attempt_index_false, |kind| {
            matches!(kind, ErrorKind::TypeError(_))
        }),
        ("index_cycle", attempt_index_cycle, |kind| {
            matches!(kind, ErrorKind::MetamethodDepthExceeded { .. })
        }),
        ("index_rust", attempt_index_rust, |kind| {
            matches!(kind, ErrorKind::HostError(_))
        }),
        ("render_table", attempt_render_table, |kind| {
            matches!(
                kind,
                ErrorKind::LibraryError(LibraryError::TostringMustReturnString)
            )
        }),
        ("render_fail", attempt_render_fail, |kind| {
            matches!(kind, ErrorKind::HostError(_))
        }),
    ];
    for (name, attempt, expected) in cases {
        for mode in [Mode::Host, Mode::Callback, Mode::FromLua] {
            let (error, notifications) = run_attempt(attempt, mode);
            assert!(expected(error.kind()), "{name} {mode:?}: {error:?}");
            // The one notification is the error the caller received.
            assert_eq!(notifications, vec![error.to_string()], "{name} {mode:?}");
            if name == "lua_error" {
                // Raised in its own frame, which locates it.
                assert_eq!(error.source_name(), Some(b"setup".as_slice()));
                let callers = usize::from(matches!(mode, Mode::FromLua));
                assert_eq!(error.stack_trace().len(), 1 + callers, "{name} {mode:?}");
            } else if let Mode::FromLua = mode {
                // Located at the script line whose call reached the callback.
                assert_eq!(error.line_num(), 2, "{name}: {error}");
                assert_eq!(error.source_name(), Some(b"swallow-caller".as_slice()));
                assert_eq!(error.stack_trace().len(), 1, "{name}: {error}");
                assert_eq!(error.stack_trace()[0].line, 2, "{name}");
            } else {
                // No Lua activation encloses the failure.
                assert!(error.stack_trace().is_empty(), "{name} {mode:?}");
            }
        }
    }
}

#[test]
fn host_api_misuse_does_not_notify_at_any_depth() {
    let cases: [(&str, Attempt); 3] = [
        ("get_table_bad_index", attempt_get_table_bad_index),
        ("get_table_missing_key", attempt_get_table_missing_key),
        ("tostring_bad_index", attempt_tostring_bad_index),
    ];
    for (name, attempt) in cases {
        for mode in [Mode::Host, Mode::Callback, Mode::FromLua] {
            let (error, notifications) = run_attempt(attempt, mode);
            assert!(
                matches!(error.kind(), ErrorKind::InvalidStackIndex { .. }),
                "{name} {mode:?}: {error:?}"
            );
            assert!(
                notifications.is_empty(),
                "{name} {mode:?}: {notifications:?}"
            );
        }
    }
}

/// Each level calls itself again through `State::call` until the native
/// call depth limit refuses the call; the deepest level handles that error.
fn recurse_until_refused(state: &mut State) -> dellingr::Result<u8> {
    state.push_rust_fn(recurse_until_refused)?;
    if let Err(error) = state.call(ArgCount::Fixed(0), RetCount::Fixed(0)) {
        state
            .user_data_mut::<Vec<Error>>()
            .ok_or_else(|| Error::without_location(ErrorKind::HostError("no slot".into())))?
            .push(error);
    }
    Ok(0)
}

#[test]
fn handled_native_call_depth_refusal_notifies_once() {
    stack::on_limit_test_stack("handled_native_call_depth_refusal", || {
        for from_lua in [false, true] {
            let (mut state, recorder) = Recorder::state();
            state.set_user_data(Vec::<Error>::new());
            if from_lua {
                state.push_rust_fn(recurse_until_refused).unwrap();
                state.set_global("recurse").unwrap();
                dellingr::Program::compile("local before = 1\nrecurse()", Some(b"depth-caller"))
                    .and_then(|program| state.load(&program))
                    .unwrap();
            } else {
                state.push_rust_fn(recurse_until_refused).unwrap();
            }
            state
                .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                .expect("the deepest level handles the refusal");
            let swallowed = std::mem::take(state.user_data_mut::<Vec<Error>>().unwrap());
            assert_eq!(swallowed.len(), 1);
            let error = &swallowed[0];
            assert!(
                matches!(error.kind(), ErrorKind::NativeCallDepthExceeded { .. }),
                "{error:?}"
            );
            assert_eq!(recorder.errors(), vec![error.to_string()]);
            if from_lua {
                assert_eq!(error.line_num(), 2);
                assert_eq!(error.source_name(), Some(b"depth-caller".as_slice()));
            }
            assert_eq!(state.get_top(), 0);
        }
    });
}
