#![cfg(feature = "snapshot")]
//! Snapshot byte stability and rendered-identity stability across save/load.

use dellingr::{ArgCount, DefaultCallbacks, RetCount, State};

fn run(state: &mut State, source: &str) {
    state.load_string(source).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
}

fn global_str(state: &mut State, name: &str) -> String {
    state.get_global(name).unwrap();
    let s = state.to_string_with_meta(-1).unwrap();
    state.pop(1).unwrap();
    s
}

fn host_fn(_state: &mut State) -> dellingr::Result<u8> {
    Ok(0)
}

#[test]
fn nan_sign_does_not_reach_snapshot_bytes() {
    // x86-64 and aarch64 produce default NaNs differing only in the sign bit.
    let save_with = |bits: u64| {
        let mut state = State::new();
        state.push_number(f64::from_bits(bits)).unwrap();
        state.set_global("n").unwrap();
        run(
            &mut state,
            "t = {}; t.v = n; local captured = n; f = function() return captured end",
        );
        state.save_state().unwrap().bytes
    };
    let positive = save_with(0x7ff8_0000_0000_0000);
    let negative = save_with(0xfff8_0000_0000_0000);
    let payload = save_with(0x7ff0_0000_0000_0001);
    assert_eq!(positive, negative);
    assert_eq!(positive, payload);

    let mut loaded = State::load_state(&positive, Box::new(DefaultCallbacks), |_| {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap();
    run(
        &mut loaded,
        "if n == n or t.v == t.v or f() == f() then error('NaN lost') end",
    );
}

#[test]
fn rust_function_held_only_by_host_keeps_its_identity() {
    let mut state = State::new();
    state
        .set_global_named_rust_fn("host_fn", "game.host_fn", host_fn)
        .unwrap();
    run(
        &mut state,
        "before = tostring(host_fn); before_p = string.format('%p', host_fn); host_fn = nil",
    );
    let before = global_str(&mut state, "before");
    let before_p = global_str(&mut state, "before_p");
    state.gc_collect();
    let save = state.save_state().unwrap();

    // The host still holds the function and hands it back after load.
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        state.set_global_named_rust_fn("handed_back", "game.host_fn", host_fn)
    })
    .unwrap();
    run(
        &mut loaded,
        "after = tostring(handed_back); after_p = string.format('%p', handed_back); fresh_p = string.format('%p', {})",
    );
    assert_eq!(global_str(&mut loaded, "after"), before);
    assert_eq!(global_str(&mut loaded, "after_p"), before_p);
    assert_ne!(global_str(&mut loaded, "fresh_p"), before_p);
}

#[test]
fn unregistered_unreachable_rust_function_identity_does_not_fail_load() {
    let mut state = State::new();
    state
        .set_global_named_rust_fn("host_fn", "game.host_fn", host_fn)
        .unwrap();
    run(
        &mut state,
        "before_p = string.format('%p', host_fn); host_fn = nil",
    );
    let before_p = global_str(&mut state, "before_p");
    state.gc_collect();
    let save = state.save_state().unwrap();

    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap();
    // The retired id is never reissued to a new value.
    run(&mut loaded, "fresh_p = string.format('%p', {})");
    assert_ne!(global_str(&mut loaded, "fresh_p"), before_p);
}

#[test]
fn nil_global_tombstones_preserve_snapshot_bytes_and_insertion_order() {
    for mut state in [State::new(), State::empty()] {
        // Host writes work with either environment. The first name must keep
        // its original position even after load and a later non-nil write.
        state.push_number(1.0).unwrap();
        state.set_global("first").unwrap();
        state.push_number(2.0).unwrap();
        state.set_global("second").unwrap();
        state.push_nil().unwrap();
        state.set_global("first").unwrap();
        let save = state.save_state().unwrap();
        let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();
        assert_eq!(loaded.save_state().unwrap().bytes, save.bytes);
        for current in [&mut state, &mut loaded] {
            current.push_number(3.0).unwrap();
            current.set_global("first").unwrap();
        }
        assert_eq!(
            loaded.save_state().unwrap().bytes,
            state.save_state().unwrap().bytes
        );
    }
}
