#![cfg(feature = "snapshot")]

use dellingr::{ArgCount, RetCount, SaveError, State};

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
        .push_rust_fn(unregistered)
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
fn unregistered_function_path_in_closed_upvalue_is_rendered() {
    let mut state = State::new();
    state
        .push_rust_fn(unregistered)
        .expect("callback fits on the stack");
    state.set_global("seed").expect("callback is installed");
    dellingr::Program::compile(
        "do local callback = seed; root = function() return callback end end",
        None,
    )
    .and_then(|program| state.load(&program))
    .expect("closure source compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("closure is created");
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
