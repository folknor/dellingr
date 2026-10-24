//! A panic that unwinds through a State's execution poisons it: every later
//! public entry point refuses with `ErrorKind::StatePoisoned`, and dropping
//! the State stays safe. Covers a panicking RustFn, a panic inside
//! `with_restricted_env`, a panic swallowed by a callback (which must abandon
//! the outer execution rather than resume it), and panics reached through a
//! metamethod and a vararg frame.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Once;

use dellingr::error::{Error, ErrorKind};
use dellingr::{ArgCount, HostCallbacks, RetCount, State};

use crate::support;

const PANIC_MESSAGE: &str = "deliberate test panic";

/// Silence only this file's deliberate panics; anything else still reports.
/// The hook is process-wide and stays installed, so it filters on this
/// module's exact payload and forwards every other panic to the hook it
/// replaced: no other test can tell it is there.
#[allow(clippy::disallowed_methods)] // One permanent payload-filtering hook, installed once.
fn quiet_deliberate_panics() {
    // process-global: guards the one-time install below.
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        // process-global: see the function doc; the default hook is kept.
        let default = std::panic::take_hook();
        // process-global: forwards every panic but this module's own.
        std::panic::set_hook(Box::new(move |info| {
            let deliberate = info
                .payload()
                .downcast_ref::<&str>()
                .is_some_and(|message| *message == PANIC_MESSAGE);
            if !deliberate {
                default(info);
            }
        }));
    });
}

fn panicking_fn(_state: &mut State) -> dellingr::Result<u8> {
    std::panic::panic_any(PANIC_MESSAGE);
}

/// Calls the Lua global `inner` and swallows the panic it raises, then
/// returns normally as though nothing happened.
fn swallowing_fn(state: &mut State) -> dellingr::Result<u8> {
    state.get_global("inner")?;
    let caught = catch_unwind(AssertUnwindSafe(|| {
        state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
    }));
    if caught.is_ok() {
        return Err(Error::without_location(ErrorKind::HostError(
            "inner was expected to panic".into(),
        )));
    }
    Ok(0)
}

fn state_with(fns: &[(&str, dellingr::RustFunc)], source: &str) -> State {
    let mut state = State::new();
    for (name, f) in fns {
        state.push_rust_fn(*f).expect("room for a function");
        state.set_global(name).expect("global install");
    }
    support::run(&mut state, source);
    state
}

fn assert_poisoned(result: dellingr::Result<()>) {
    let error = result.expect_err("a poisoned State must refuse");
    assert!(
        matches!(error.kind(), ErrorKind::StatePoisoned),
        "unexpected error kind: {:?}",
        error.kind()
    );
}

/// Every refusal a poisoned State owes, plus the operations that must stay
/// safe on it.
fn assert_refuses_everything(state: &mut State) {
    assert!(state.is_poisoned());
    assert_poisoned(state.call(ArgCount::Fixed(0), RetCount::Fixed(0)));
    assert_poisoned(
        dellingr::Program::compile("return 1", None).and_then(|program| state.load(&program)),
    );
    assert_poisoned(state.push_nil());
    assert_poisoned(state.push_number(1.0));
    assert_poisoned(state.push_string("x"));
    assert_poisoned(state.get_global("print"));
    assert_poisoned(state.set_global("x"));
    assert_poisoned(state.new_table());
    assert_poisoned(state.pop(0));
    assert_poisoned(state.set_top(0));
    assert_poisoned(state.get_table(1));
    assert_poisoned(state.consume_cost(1));
    assert_poisoned(state.anchor().map(|_| ()));
    assert_poisoned(state.tostring_bytes(1).map(|_| ()));
    assert_poisoned(state.open_libs());
    assert_poisoned(state.to_number(1).map(|_| ()));
    assert_poisoned(state.to_bytes(1).map(|_| ()));
    assert_poisoned(state.try_to_boolean(1).map(|_| ()));
    assert_poisoned(state.try_typ(1).map(|_| ()));
    assert_poisoned(state.try_raw_equal(1, 1).map(|_| ()));
    assert_poisoned(state.raw_len(1).map(|_| ()));
    let restricted = state.with_restricted_env(&[], |_| -> u8 { panic!("must not run") });
    assert_poisoned(restricted.map(|_| ()));
    #[cfg(feature = "snapshot")]
    assert!(matches!(
        state.save_state(),
        Err(dellingr::SaveError::StatePoisoned)
    ));
    // Collection is a no-op rather than a walk of abandoned roots, and
    // reading counters stays safe.
    state.gc_collect();
    let _ = state.cost_used();
}

#[test]
fn panicking_rust_fn_poisons_the_state() {
    quiet_deliberate_panics();
    let mut state = state_with(&[("boom", panicking_fn)], "");
    state.get_global("boom").expect("boom is installed");
    // The abandoned callback frame keeps this argument on the stack, so the
    // readers in assert_refuses_everything would succeed without the check.
    state.push_string("abc").expect("room for an argument");
    let caught = catch_unwind(AssertUnwindSafe(|| {
        state.call(ArgCount::Fixed(1), RetCount::Fixed(0))
    }));
    let payload = caught.expect_err("the host sees its own panic");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&PANIC_MESSAGE));
    assert_refuses_everything(&mut state);
    drop(state);
}

#[test]
fn panic_through_nested_lua_frames_poisons_the_state() {
    quiet_deliberate_panics();
    let mut state = state_with(
        &[("boom", panicking_fn)],
        "function outer() local captured = 1; local f = function() return captured end; boom(); return f end",
    );
    state.get_global("outer").expect("outer is defined");
    let caught = catch_unwind(AssertUnwindSafe(|| {
        state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
    }));
    assert!(caught.is_err());
    assert_refuses_everything(&mut state);
}

#[test]
fn panic_inside_restricted_env_poisons_the_state() {
    quiet_deliberate_panics();
    let mut state = State::new();
    let caught = catch_unwind(AssertUnwindSafe(|| {
        state.with_restricted_env(&["print"], |state| {
            state.gc_collect();
            std::panic::panic_any(PANIC_MESSAGE);
        })
    }));
    assert!(caught.is_err(), "the panic must propagate");
    assert_refuses_everything(&mut state);
}

#[test]
fn panic_in_restricted_env_callee_poisons_the_state() {
    quiet_deliberate_panics();
    let mut state = state_with(&[("boom", panicking_fn)], "");
    let caught = catch_unwind(AssertUnwindSafe(|| {
        state.with_restricted_env(&["boom"], |state| {
            state.get_global("boom")?;
            state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
        })
    }));
    assert!(caught.is_err(), "the panic must propagate");
    assert_refuses_everything(&mut state);
}

#[test]
fn restricted_env_closure_that_swallows_a_panic_is_refused() {
    quiet_deliberate_panics();
    let mut state = state_with(&[("boom", panicking_fn)], "");
    let result = state.with_restricted_env(&["boom"], |state| {
        state.get_global("boom").expect("boom is whitelisted");
        let caught = catch_unwind(AssertUnwindSafe(|| {
            state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
        }));
        caught.is_err()
    });
    assert_poisoned(result.map(|_| ()));
    assert_refuses_everything(&mut state);
}

/// How many times `after` ran, kept in the State's user data.
#[derive(Default)]
struct AfterSwallow(usize);

fn count_after_swallow(state: &mut State) -> dellingr::Result<u8> {
    state
        .user_data_mut::<AfterSwallow>()
        .expect("the test installs the counter")
        .0 += 1;
    Ok(0)
}

#[test]
fn swallowed_panic_abandons_the_outer_execution() {
    quiet_deliberate_panics();
    // `inner` leaves an open upvalue in its abandoned frame. `swallow` is
    // reached through `__call`, whose error path truncates the call frame:
    // if the swallowed panic were propagated as an ordinary error, that
    // truncation would drop the slot the open upvalue points at before the
    // outer frame's cleanup closes it. Abandonment must instead reach the
    // host as StatePoisoned, with no later Lua instruction run.
    let mut state = state_with(
        &[
            ("boom", panicking_fn),
            ("swallow", swallowing_fn),
            ("after", count_after_swallow),
        ],
        "
        function inner()
            local captured = 1
            local f = function() return captured end
            boom()
            return f
        end
        callable = setmetatable({}, { __call = function(self) swallow() end })
        function outer()
            local mine = 2
            local g = function() return mine end
            callable()
            after()
            return g
        end
        ",
    );
    state.set_user_data(AfterSwallow::default());
    state.get_global("outer").expect("outer is defined");
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    assert_poisoned(result);
    assert_eq!(
        state
            .user_data::<AfterSwallow>()
            .expect("the counter is installed")
            .0,
        0,
        "execution resumed after the swallowed panic"
    );
    assert_refuses_everything(&mut state);
}

#[test]
fn swallowed_panic_then_callback_error_is_still_abandoned() {
    quiet_deliberate_panics();
    fn swallow_then_fail(state: &mut State) -> dellingr::Result<u8> {
        swallowing_fn(state)?;
        Err(Error::without_location(ErrorKind::HostError(
            "callback error after a swallowed panic".into(),
        )))
    }
    let mut state = state_with(
        &[("boom", panicking_fn), ("swallow", swallow_then_fail)],
        "function inner() boom() end
         function outer() local x = 1; local f = function() return x end; swallow(); return f end",
    );
    state.get_global("outer").expect("outer is defined");
    assert_poisoned(state.call(ArgCount::Fixed(0), RetCount::Fixed(0)));
    assert_refuses_everything(&mut state);
}

struct PanickingOnError;

impl HostCallbacks for PanickingOnError {
    fn on_print(&mut self, _source: Option<&[u8]>, _line: u32, _message: &[u8]) {}

    fn on_error(&mut self, _error: &Error) {
        std::panic::panic_any(PANIC_MESSAGE);
    }
}

fn failing_fn(_state: &mut State) -> dellingr::Result<u8> {
    Err(Error::without_location(ErrorKind::HostError(
        "deliberate callback error".into(),
    )))
}

#[test]
fn panicking_on_error_from_a_host_tostring_poisons_the_state() {
    quiet_deliberate_panics();
    // The callback returns an error normally; the panic comes from on_error,
    // raised after the callback's own unwind guard, so only the public
    // boundary of tostring_bytes can mark it.
    let mut state = State::with_callbacks(Box::new(PanickingOnError));
    state.push_rust_fn(failing_fn).expect("room for a function");
    state.set_global("fail").expect("global install");
    support::run(&mut state, "t = setmetatable({}, { __tostring = fail })");
    state.get_global("t").expect("t is defined");
    let caught = catch_unwind(AssertUnwindSafe(|| state.tostring_bytes(1)));
    assert!(caught.is_err(), "the on_error panic must propagate");
    assert_refuses_everything(&mut state);
}

#[cfg(feature = "snapshot")]
#[test]
fn load_setup_that_swallows_a_panic_is_refused() {
    quiet_deliberate_panics();
    let save = State::new().save_state().expect("a fresh State saves");
    let result = State::load_state(
        &save.bytes,
        Box::new(dellingr::DefaultCallbacks),
        |state| -> dellingr::Result<()> {
            state.push_rust_fn(panicking_fn)?;
            let caught = catch_unwind(AssertUnwindSafe(|| {
                state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
            }));
            assert!(caught.is_err());
            Ok(())
        },
    );
    assert!(matches!(result, Err(dellingr::LoadError::StatePoisoned)));
}

#[test]
fn panic_through_a_host_table_access_metamethod_poisons_the_state() {
    quiet_deliberate_panics();
    let mut state = state_with(
        &[("boom", panicking_fn)],
        "t = setmetatable({}, { __index = function(_, _) boom() end })",
    );
    state.get_global("t").expect("t is defined");
    state.push_string("missing").expect("room for the key");
    let caught = catch_unwind(AssertUnwindSafe(|| state.get_table(1)));
    assert!(caught.is_err());
    assert_refuses_everything(&mut state);
}

#[test]
fn panic_through_a_vararg_frame_poisons_the_state() {
    quiet_deliberate_panics();
    let mut state = state_with(&[("boom", panicking_fn)], "function va(...) boom(...) end");
    state.get_global("va").expect("va is defined");
    state.push_number(1.0).expect("room for an argument");
    state.push_number(2.0).expect("room for an argument");
    let caught = catch_unwind(AssertUnwindSafe(|| {
        state.call(ArgCount::Fixed(2), RetCount::Fixed(0))
    }));
    assert!(caught.is_err());
    assert_refuses_everything(&mut state);
}

#[test]
fn an_unpoisoned_state_is_unaffected() {
    let mut state = State::new();
    assert!(!state.is_poisoned());
    support::eval(&mut state, "return 1 + 1", 1);
    assert_eq!(state.to_number(-1).expect("a number"), 2.0);
}
