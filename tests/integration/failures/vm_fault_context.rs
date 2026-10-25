//! Internal instruction context is diagnostic metadata, not rendered Lua text.
//! Only internal errors carry it; the in-crate unit test
//! `internal_fault_keeps_innermost_instruction` covers that case, because only
//! dellingr can construct an internal error. Naming the unit test is the
//! intended pointer; if it is renamed, update this comment.

use crate::support::PushNewRustFn;
use dellingr::error::{Error, ErrorKind};
use dellingr::{ArgCount, Program, RetCount, State};

fn host_failure(_state: &mut State) -> dellingr::Result<u8> {
    Err(Error::without_location(ErrorKind::HostError(
        "test callback failure".into(),
    )))
}

#[test]
fn host_callback_failure_has_no_vm_fault() {
    let mut state = State::new();
    state.push_new_rust_fn(host_failure).unwrap();
    state.set_global("fail_host").unwrap();
    let program = Program::compile(
        "local function inner() return fail_host() end\n\
         local function outer() return inner() end\n\
         return outer()",
        None,
    )
    .unwrap();
    state.load(&program).unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("callback fails through nested Lua frames");
    // A host failure has a Lua traceback but is no evidence of an internal
    // instruction fault.
    assert!(error.vm_fault().is_none());
    assert_eq!(error.stack_trace()[0].line, 1);
    assert_eq!(error.kind().to_string(), "test callback failure");
    assert!(matches!(error.kind(), ErrorKind::HostError(_)));
}

#[test]
fn host_preflight_and_lua_errors_have_no_vm_fault() {
    let mut state = State::new();
    let error = state
        .call(ArgCount::Dynamic, RetCount::Fixed(0))
        .expect_err("host has no dynamic call marker");
    assert!(error.vm_fault().is_none());
    let program = Program::compile("error('ordinary Lua error')", None).unwrap();
    state.load(&program).unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("script errors");
    assert!(error.vm_fault().is_none());
}
