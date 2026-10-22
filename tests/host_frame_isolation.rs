//! A host callback must not reach below its own frame: a Lua caller's
//! temporaries and its registers belong to the calling Lua frame. Misuse must surface as an error and leave
//! the caller's frame intact.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

fn run(state: &mut State, src: &str) {
    dellingr::Program::compile(src, None)
        .and_then(|program| state.load(&program))
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
}

fn global_number(state: &mut State, name: &str) -> f64 {
    state.get_global(name).unwrap();
    let n = state.to_number(-1).unwrap();
    state.pop(1).unwrap();
    n
}

fn report(state: &mut State, name: &str, flag: bool) -> dellingr::Result<()> {
    state.push_number(if flag { 1.0 } else { 0.0 })?;
    state.set_global(name)
}

// Runs inside argument evaluation of `outer(1, g())`, while the outer frame's
// Dynamic call base is pending.
fn dynamic_call_from_callback(state: &mut State) -> dellingr::Result<u8> {
    let rejected = match state.call(ArgCount::Dynamic, RetCount::Fixed(0)) {
        Err(e) => matches!(
            e.kind(),
            ErrorKind::DynamicArgumentsUnsupported {
                api: dellingr::error::HostCallApi::Call
            }
        ),
        Ok(()) => false,
    };
    report(state, "rejected", rejected)?;
    Ok(0)
}

#[test]
fn host_dynamic_call_does_not_consume_callers_call_base() {
    let mut state = State::new();
    state.push_rust_fn(dynamic_call_from_callback).unwrap();
    state.set_global("g").unwrap();
    run(
        &mut state,
        "local function outer(...) return select('#', ...) end
         local a = 7
         result = outer(1, g())
         after = a",
    );
    assert_eq!(global_number(&mut state, "rejected"), 1.0);
    assert_eq!(global_number(&mut state, "result"), 1.0);
    assert_eq!(global_number(&mut state, "after"), 7.0);
}

#[test]
fn host_dynamic_call_is_rejected_at_top_level() {
    let mut state = State::new();
    state.push_number(1.0).unwrap();
    let err = state
        .call(ArgCount::Dynamic, RetCount::Fixed(0))
        .expect_err("hosts own no call base");
    assert!(matches!(
        err.kind(),
        ErrorKind::DynamicArgumentsUnsupported {
            api: dellingr::error::HostCallApi::Call
        }
    ));
    assert_eq!(state.get_top(), 1);
}

// Called with no arguments, so its visible frame is empty.
fn set_global_on_empty_frame(state: &mut State) -> dellingr::Result<u8> {
    let rejected = match state.set_global("stolen") {
        Err(e) => matches!(e.kind(), ErrorKind::InvalidStackIndex { .. }),
        Ok(()) => false,
    };
    report(state, "rejected", rejected)?;
    Ok(0)
}

#[test]
fn set_global_on_empty_callback_frame_is_rejected() {
    let mut state = State::new();
    state.push_rust_fn(set_global_on_empty_frame).unwrap();
    state.set_global("g").unwrap();
    run(
        &mut state,
        "local a = 5
         local b = 6
         g()
         after_a = a
         after_b = b",
    );
    assert_eq!(global_number(&mut state, "rejected"), 1.0);
    assert_eq!(global_number(&mut state, "after_a"), 5.0);
    assert_eq!(global_number(&mut state, "after_b"), 6.0);
    state.get_global("stolen").unwrap();
    assert_eq!(state.typ(-1), dellingr::LuaType::Nil);
}

#[test]
fn set_global_on_empty_stack_errors_instead_of_panicking() {
    let mut state = State::new();
    let err = state.set_global("x").expect_err("nothing to pop");
    assert!(matches!(err.kind(), ErrorKind::InvalidStackIndex { .. }));
    assert_eq!(state.get_top(), 0);
}

// Asks call_anchor for one argument with an empty visible frame.
fn call_anchor_short_of_args(state: &mut State) -> dellingr::Result<u8> {
    state.get_global("h")?;
    let h = state.anchor_function()?;
    let rejected = match state.call_anchor(&h, ArgCount::Fixed(1), RetCount::Fixed(0)) {
        Err(e) => matches!(e.kind(), ErrorKind::InvalidStackIndex { .. }),
        Ok(()) => false,
    };
    let untouched = state.get_top() == 0;
    report(state, "rejected", rejected && untouched)?;
    Ok(0)
}

#[test]
fn call_anchor_checks_args_against_the_visible_frame() {
    let mut state = State::new();
    state.push_rust_fn(call_anchor_short_of_args).unwrap();
    state.set_global("g").unwrap();
    run(
        &mut state,
        "function h() end
         local a = 3
         local b = 4
         g()
         after_a = a
         after_b = b",
    );
    assert_eq!(global_number(&mut state, "rejected"), 1.0);
    assert_eq!(global_number(&mut state, "after_a"), 3.0);
    assert_eq!(global_number(&mut state, "after_b"), 4.0);
}
