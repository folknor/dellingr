//! `State::open_libs` is all-or-nothing against the snapshot function registry.

use dellingr::error::ErrorKind;
use dellingr::{LuaType, State};

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
    // base and math; without the dry run those would already be installed.
    state
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
    state
        .register_rust_fn("string.find", host_print)
        .expect("the original registration must survive rejection");
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
    state
        .open_libs()
        .expect("same ids map to the same functions");
    assert_eq!(global_type(&mut state, "string"), LuaType::Table);
}
