use dellingr::{SaveError, State};

use crate::support;
use crate::support::PushNewRustFn;

fn unregistered(_state: &mut State) -> dellingr::Result<u8> {
    Ok(0)
}

#[test]
fn nested_unregistered_function_path_is_rendered_from_its_root() {
    let mut state = State::new();
    state.new_table().expect("root table allocates");
    state.push_bytes("child").expect("key string allocates");
    state.new_table().expect("child table allocates");
    state
        .set_table_raw(1)
        .expect("child table is stored in root");

    state.push_bytes("child").expect("key string allocates");
    state
        .get_table_raw(1)
        .expect("child table is read from root");
    state
        .push_bytes("callback")
        .expect("callback key string allocates");
    state
        .push_new_rust_fn(unregistered)
        .expect("callback fits on the stack");
    state
        .set_table_raw(-3)
        .expect("callback is stored in child table");
    state.pop(1).expect("child table is no longer needed");
    state.set_global("root").expect("root table is installed");

    let error = state
        .save_state()
        .expect_err("unregistered function is rejected");
    assert_eq!(
        error,
        SaveError::UnregisteredFunction {
            reachable_from: "global root[0][0]".to_owned(),
        }
    );
    assert_eq!(
        error.to_string(),
        "unregistered Rust function reachable from global root[0][0]"
    );
}

#[test]
fn unregistered_function_in_unnameable_global_slot_has_global_table_path() {
    let mut state = State::new();
    state.get_global("_G").expect("global table is available");
    let key = [0xff];
    state
        .push_bytes(&key[..])
        .expect("non-UTF-8 global key allocates");
    state
        .push_new_rust_fn(unregistered)
        .expect("callback fits on the stack");
    state
        .set_table_raw(-3)
        .expect("callback is stored under the non-UTF-8 key");
    state.pop(1).expect("global table is no longer needed");

    let error = state
        .save_state()
        .expect_err("unregistered function is rejected");
    let reachable_from = match error {
        SaveError::UnregisteredFunction { reachable_from } => reachable_from,
        error => panic!("unexpected save error: {error}"),
    };
    assert!(reachable_from.starts_with("global table entry["));
    assert!(reachable_from.ends_with("].value"));
    assert!(!reachable_from.starts_with("environment _G"));
}

#[test]
fn unregistered_function_used_as_global_table_key_has_key_path() {
    let mut state = State::new();
    state.get_global("_G").expect("global table is available");
    state
        .push_new_rust_fn(unregistered)
        .expect("callback fits on the stack");
    state.push_boolean(true).expect("value fits on the stack");
    state
        .set_table_raw(-3)
        .expect("callback is stored as a global table key");
    state.pop(1).expect("global table is no longer needed");

    let error = state
        .save_state()
        .expect_err("unregistered function is rejected");
    let reachable_from = match error {
        SaveError::UnregisteredFunction { reachable_from } => reachable_from,
        error => panic!("unexpected save error: {error}"),
    };
    assert!(reachable_from.starts_with("global table entry["));
    assert!(reachable_from.ends_with("].key"));
}

#[test]
fn unregistered_function_path_in_closed_upvalue_is_rendered() {
    let mut state = State::new();
    state
        .push_new_rust_fn(unregistered)
        .expect("callback fits on the stack");
    state.set_global("seed").expect("callback is installed");
    support::run(
        &mut state,
        "do local callback = seed; root = function() return callback end end",
    );
    state.push_nil().expect("nil fits on the stack");
    state
        .set_global("seed")
        .expect("temporary callback global is cleared");

    let error = state
        .save_state()
        .expect_err("captured callback is rejected");
    assert_eq!(
        error,
        SaveError::UnregisteredFunction {
            reachable_from: "global root.upvalue[0]".to_owned(),
        }
    );
}
