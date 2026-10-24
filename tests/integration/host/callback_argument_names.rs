//! Argument errors raised by host callbacks name the callback, whether Lua,
//! a generic `for` or the host called it, and nested dispatch restores the
//! outer name.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

use crate::support;
use crate::support::PushNewRustFn;

fn check_number(state: &mut State) -> dellingr::Result<u8> {
    state.check_type(1, dellingr::LuaType::Number)?;
    Ok(0)
}

fn install_callback(state: &mut State) {
    state.push_new_rust_fn(check_number).expect("callback fits");
    state.set_global("host_number").unwrap();
}

#[test]
fn host_argument_checks_name_direct_lua_and_iterator_callbacks() {
    for code in [
        "return host_number(false)",
        "for value in host_number, false, nil do end",
    ] {
        let mut state = State::new();
        install_callback(&mut state);
        support::load(&mut state, code);
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("callback rejects boolean");
        let ErrorKind::ArgError(argument) = error.kind() else {
            panic!("expected callback argument error");
        };
        assert_eq!(
            argument.to_string(),
            "bad argument #1 to 'host_number' (number expected, got boolean)"
        );
        assert_eq!(state.get_top(), 0);
    }

    let mut state = State::new();
    install_callback(&mut state);
    state.get_global("host_number").expect("callback exists");
    state.push_boolean(false).expect("argument fits");
    let error = state
        .call(ArgCount::Fixed(1), RetCount::Fixed(0))
        .expect_err("callback rejects boolean");
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected callback argument error");
    };
    assert_eq!(argument.func_name(), Some("host_number"));
    let error = state
        .check_type(1, dellingr::LuaType::Number)
        .expect_err("the callback context was restored and stack is empty");
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected missing argument error");
    };
    assert_eq!(argument.func_name(), None);
}

fn nested_check_number(state: &mut State) -> dellingr::Result<u8> {
    state.get_global("host_number")?;
    state.push_number(1.0)?;
    state.call(ArgCount::Fixed(1), RetCount::Fixed(0))?;
    state.check_type(1, dellingr::LuaType::Number)?;
    Ok(0)
}

#[test]
fn nested_native_dispatch_restores_outer_argument_name() {
    let mut state = State::new();
    install_callback(&mut state);
    state
        .push_new_rust_fn(nested_check_number)
        .expect("callback fits");
    state.set_global("outer_number").unwrap();
    state
        .get_global("outer_number")
        .expect("outer callback exists");
    state.push_boolean(false).expect("argument fits");
    let error = state
        .call(ArgCount::Fixed(1), RetCount::Fixed(0))
        .expect_err("outer callback rejects boolean after inner callback succeeds");
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected outer callback argument error");
    };
    assert_eq!(argument.func_name(), Some("outer_number"));
}

#[cfg(feature = "snapshot")]
#[test]
fn snapshot_registered_callback_uses_its_id_without_a_global() {
    let mut state = State::new();
    let callback = state
        .register_rust_fn("host.number", check_number)
        .expect("the id is new");
    state
        .push_rust_fn(callback)
        .expect("registered callback fits");
    state.push_boolean(false).expect("argument fits");
    let error = state
        .call(ArgCount::Fixed(1), RetCount::Fixed(0))
        .expect_err("callback rejects boolean");
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected registered callback argument error");
    };
    assert_eq!(argument.func_name(), Some("host.number"));
}
