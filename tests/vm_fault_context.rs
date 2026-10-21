//! Internal instruction context is diagnostic metadata, not rendered Lua text.

use dellingr::error::{Error, ErrorKind};
use dellingr::{ArgCount, RetCount, State};

fn internal_failure(_state: &mut State) -> dellingr::Result<u8> {
    Err(Error::without_location(ErrorKind::InternalError(
        "test callback failure".into(),
    )))
}

#[test]
fn internal_callback_fault_keeps_innermost_instruction() {
    let mut state = State::new();
    state.push_rust_fn(internal_failure).unwrap();
    state.set_global("fail_internal").unwrap();
    state
        .load_string(
            "local function inner() return fail_internal() end\n\
             local function outer() return inner() end\n\
             return outer()",
        )
        .unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("callback fails through nested Lua frames");
    let fault = error.vm_fault().expect("Lua dispatch supplies context");
    // The trace also starts at inner; preserving this instruction prevents
    // outer frame unwinding from overwriting it with its own call site.
    assert_eq!(fault.function_name, error.stack_trace()[0].function_name);
    assert_eq!(error.stack_trace()[0].line, 1);
    assert_eq!(
        error.kind().to_string(),
        "internal error: test callback failure"
    );
    assert!(matches!(error.kind(), ErrorKind::InternalError(_)));
}

#[test]
fn host_preflight_and_lua_errors_have_no_vm_fault() {
    let mut state = State::new();
    let error = state
        .call(ArgCount::Dynamic, RetCount::Fixed(0))
        .expect_err("host has no dynamic call marker");
    assert!(error.vm_fault().is_none());
    state.load_string("error('ordinary Lua error')").unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("script errors");
    assert!(error.vm_fault().is_none());
}

#[test]
fn legacy_internal_kinds_keep_exact_prefixes() {
    assert_eq!(
        ErrorKind::InternalError("detail".into()).to_string(),
        "internal error: detail"
    );
}
