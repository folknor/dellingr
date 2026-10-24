//! `State::open_libs` is all-or-nothing against the snapshot function registry.

use dellingr::error::ErrorKind;
use dellingr::{LuaType, State};

use crate::support;

fn host_print(_state: &mut State) -> dellingr::Result<u8> {
    Ok(0)
}

fn global_type(state: &mut State, name: &str) -> LuaType {
    state.get_global(name).expect("room for one value");
    let ty = state.typ(-1);
    state.pop(1).expect("one value was pushed");
    ty
}

#[test]
fn conflicting_preregistered_id_leaves_state_untouched() {
    let mut state = State::raw();
    // "string.find" is registered by the string library, which opens after
    // base and math; without the preflight those would already be installed.
    let host = state
        .register_rust_fn("string.find", host_print)
        .expect("fresh registry");
    let top = state.get_top();
    let heap_size = state.heap_size();

    let error = state.open_libs().expect_err("id conflicts");
    assert!(
        matches!(error.kind(), ErrorKind::DuplicateFunctionRegistration { id } if id == "string.find")
    );
    assert_eq!(state.get_top(), top);
    assert_eq!(state.heap_size(), heap_size);
    assert_eq!(global_type(&mut state, "print"), LuaType::Nil);
    assert_eq!(global_type(&mut state, "math"), LuaType::Nil);
    assert_eq!(global_type(&mut state, "string"), LuaType::Nil);
    assert_eq!(global_type(&mut state, "table"), LuaType::Nil);
    assert_eq!(global_type(&mut state, "_G"), LuaType::Nil);
    // Base registration would have happened before the conflicting string id.
    state
        .register_rust_fn("print", host_print)
        .expect("failed setup must not register earlier functions");
    assert_eq!(
        state.registered_rust_fn("string.find"),
        Some(host),
        "the original registration must survive rejection"
    );
}

#[test]
fn the_smallest_conflicting_id_is_reported() {
    let mut state = State::raw();
    for id in ["table.unpack", "ipairs.iter", "print"] {
        state.register_rust_fn(id, host_print).unwrap();
    }
    let error = state.open_libs().expect_err("ids conflict");
    assert!(
        matches!(error.kind(), ErrorKind::DuplicateFunctionRegistration { id } if id == "ipairs.iter")
    );
}

#[test]
fn conflicting_hidden_iterator_id_leaves_existing_environment_untouched() {
    let mut state = State::raw();
    state.push_number(42.0).unwrap();
    state.set_global("print").unwrap();
    state.push_string("preserved stack value").unwrap();
    state
        .register_rust_fn("string.gmatch.empty_iter", host_print)
        .unwrap();
    let heap_size = state.heap_size();
    let error = state.open_libs().expect_err("hidden iterator id conflicts");
    assert!(
        matches!(error.kind(), ErrorKind::DuplicateFunctionRegistration { id }
        if id == "string.gmatch.empty_iter")
    );
    assert_eq!(state.heap_size(), heap_size);
    assert_eq!(state.get_top(), 1);
    assert_eq!(state.to_string(1).unwrap(), "preserved stack value");
    state.get_global("print").unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 42.0);
    state.pop(1).unwrap();
    state
        .register_rust_fn("print", host_print)
        .expect("earlier registrations must not leak");
}

#[test]
fn non_conflicting_preregistration_opens_normally() {
    let mut state = State::raw();
    state
        .register_rust_fn("host.print", host_print)
        .expect("fresh registry");
    state.open_libs().expect("no id overlaps the stdlib");
    assert_eq!(global_type(&mut state, "print"), LuaType::Function);
}

#[test]
fn reopening_libraries_accepts_existing_stdlib_registrations() {
    let mut state = State::new();
    let print = state.registered_rust_fn("print").expect("open_libs ran");
    support::run(&mut state, "old_print, old_next = print, next");
    state
        .open_libs()
        .expect("each id is bound to its own catalog entry");
    assert_eq!(global_type(&mut state, "string"), LuaType::Table);
    // Reopening binds the same entries: values from before compare equal.
    assert_eq!(state.registered_rust_fn("print"), Some(print));
    support::run(
        &mut state,
        "if old_print ~= print or old_next ~= next then error('reopen changed identity') end",
    );
}
