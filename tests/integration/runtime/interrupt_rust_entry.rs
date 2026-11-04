//! Rust entry must observe a request even without a Lua call or backedge.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

use crate::support;
use crate::support::PushNewRustFn;

fn stop(state: &mut State) -> dellingr::Result<u8> {
    state.interrupt().trigger();
    Ok(0)
}

fn touch(state: &mut State) -> dellingr::Result<u8> {
    state.push_boolean(true)?;
    state.set_global("reached")?;
    Ok(0)
}

fn state() -> State {
    let mut state = State::new();
    let functions: [(&str, dellingr::RustFunc); 2] = [("stop", stop), ("touch", touch)];
    for (name, function) in functions {
        state.push_named_rust_fn(name, function).unwrap();
        state.set_global(name).unwrap();
    }
    state
}

#[test]
fn pending_interrupt_refuses_rust_entries_from_lua() {
    for source in [
        "stop(); touch()",
        "local t = setmetatable({}, {__call = touch}); stop(); t()",
        "local t = setmetatable({}, {__index = touch}); stop(); local x = t.x",
        "local t = setmetatable({}, {__newindex = touch}); stop(); t.x = 1",
        "local t = setmetatable({}, {__len = touch}); stop(); local x = #t",
        "stop(); for x in touch do end",
    ] {
        let mut state = state();
        let error = support::try_run(&mut state, source).expect_err("Rust entry must refuse");
        assert!(
            matches!(error.kind(), ErrorKind::Interrupted),
            "{source}: {error}"
        );
        assert_eq!(state.get_top(), 0, "{source}");
        state.get_global("reached").unwrap();
        assert_eq!(state.typ(-1), dellingr::LuaType::Nil, "{source}");
        state.pop(1).unwrap();
        state.interrupt().reset();
        support::run(&mut state, "touch()");
        state.get_global("reached").unwrap();
        assert!(state.to_boolean(-1));
    }
}

#[test]
fn pending_interrupt_refuses_rust_entry_from_host() {
    let mut state = state();
    state.get_global("touch").unwrap();
    state.interrupt().trigger();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("Rust entry must refuse");
    assert!(matches!(error.kind(), ErrorKind::Interrupted));
    assert_eq!(state.get_top(), 0);
    state.get_global("reached").unwrap();
    assert_eq!(state.typ(-1), dellingr::LuaType::Nil);
}
