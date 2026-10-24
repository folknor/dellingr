//! Snapshot byte stability and rendered-identity stability across save/load.

use dellingr::{DefaultCallbacks, State};

use crate::support;

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
        support::run(
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

    let mut loaded =
        State::load_state(&positive, Box::new(DefaultCallbacks), support::no_setup).unwrap();
    support::run(
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
    support::run(
        &mut state,
        "before = tostring(host_fn); before_p = string.format('%p', host_fn); host_fn = nil",
    );
    let before = support::global_string(&mut state, "before");
    let before_p = support::global_string(&mut state, "before_p");
    state.gc_collect();
    let save = state.save_state().unwrap();

    // The host still holds the function and hands it back after load.
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        state
            .set_global_named_rust_fn("handed_back", "game.host_fn", host_fn)
            .map(drop)
    })
    .unwrap();
    support::run(
        &mut loaded,
        "after = tostring(handed_back); after_p = string.format('%p', handed_back); fresh_p = string.format('%p', {})",
    );
    assert_eq!(support::global_string(&mut loaded, "after"), before);
    assert_eq!(support::global_string(&mut loaded, "after_p"), before_p);
    assert_ne!(support::global_string(&mut loaded, "fresh_p"), before_p);
}

#[test]
fn unregistered_unreachable_rust_function_identity_does_not_fail_load() {
    let mut state = State::new();
    state
        .set_global_named_rust_fn("host_fn", "game.host_fn", host_fn)
        .unwrap();
    support::run(
        &mut state,
        "before_p = string.format('%p', host_fn); host_fn = nil",
    );
    let before_p = support::global_string(&mut state, "before_p");
    state.gc_collect();
    let save = state.save_state().unwrap();

    let mut loaded =
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup).unwrap();
    // The retired id is never reissued to a new value.
    support::run(&mut loaded, "fresh_p = string.format('%p', {})");
    assert_ne!(support::global_string(&mut loaded, "fresh_p"), before_p);
}

#[test]
fn nil_global_tombstones_preserve_snapshot_bytes_and_insertion_order() {
    for mut state in [State::new(), State::raw()] {
        // Host writes work with either environment. The first name must keep
        // its original position even after load and a later non-nil write.
        state.push_number(1.0).unwrap();
        state.set_global("first").unwrap();
        state.push_number(2.0).unwrap();
        state.set_global("second").unwrap();
        state.push_nil().unwrap();
        state.set_global("first").unwrap();
        let save = state.save_state().unwrap();
        let mut loaded =
            State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup).unwrap();
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

/// Shared initialization for a fresh State and its loads: setup Lua that
/// leaves a tombstone, then a seal (deferred to the saved phase on load).
fn sealing_setup(state: &mut State) -> Result<(), std::convert::Infallible> {
    support::run(state, "configured = 1; scratch = 1; scratch = nil");
    state.seal_globals();
    Ok(())
}

fn churn(state: &mut State, tag: &str) {
    support::run(
        state,
        format!(
            "for i = 1, 300 do _G['{tag}' .. i] = i; _G['{tag}' .. i] = nil end
             {tag}_live = 1"
        ),
    );
}

#[test]
fn sealed_global_compaction_continues_identically_after_load() {
    let mut state = State::new();
    sealing_setup(&mut state).unwrap();
    support::run(&mut state, "print = nil; configured = nil; early = 1");
    churn(&mut state, "a");
    let save = state.save_state().unwrap();

    let mut loaded =
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), sealing_setup).unwrap();
    assert!(loaded.globals_sealed());
    assert_eq!(loaded.save_state().unwrap().bytes, save.bytes);

    for current in [&mut state, &mut loaded] {
        churn(current, "b");
        support::run(current, "early = nil");
        churn(current, "c");
        support::run(current, "early = 2");
        // Names setup installs stay deleted: their tombstones are pinned,
        // so no compaction drops them and no load reinstalls them.
        assert_eq!(support::global_string(current, "print"), "nil");
        assert_eq!(support::global_string(current, "configured"), "nil");
    }
    let continued = loaded.save_state().unwrap().bytes;
    assert_eq!(continued, state.save_state().unwrap().bytes);

    // A save of the loaded State reloads and continues identically too.
    let mut reloaded =
        State::load_state(&continued, Box::new(DefaultCallbacks), sealing_setup).unwrap();
    assert!(reloaded.globals_sealed());
    assert_eq!(reloaded.save_state().unwrap().bytes, continued);
    for current in [&mut state, &mut reloaded] {
        support::run(current, "early = nil");
        churn(current, "d");
    }
    assert_eq!(
        reloaded.save_state().unwrap().bytes,
        state.save_state().unwrap().bytes
    );
}

#[test]
fn unsealed_save_loads_unsealed_even_if_setup_seals() {
    let mut state = State::new();
    support::run(&mut state, "configured = 1; scratch = 1; scratch = nil");
    churn(&mut state, "a");
    let save = state.save_state().unwrap();

    let mut loaded =
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), sealing_setup).unwrap();
    assert!(!loaded.globals_sealed());
    assert_eq!(loaded.save_state().unwrap().bytes, save.bytes);
    for current in [&mut state, &mut loaded] {
        churn(current, "b");
    }
    assert_eq!(
        loaded.save_state().unwrap().bytes,
        state.save_state().unwrap().bytes
    );
}

#[test]
fn a_pinned_deletion_survives_a_setup_that_drops_the_name() {
    // `configured` is installed during initialization, then deleted, and its
    // tombstone compacts away. A load whose setup no longer installs it, and
    // the immediate resave, must still remember the deletion, so a later
    // setup that installs it again does not resurrect it.
    let mut state = State::new();
    sealing_setup(&mut state).unwrap();
    support::run(&mut state, "configured = nil");
    churn(&mut state, "a");
    let save = state.save_state().unwrap();
    let lean = |state: &mut State| {
        state.seal_globals();
        Ok::<(), std::convert::Infallible>(())
    };
    let loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), lean).unwrap();
    let resaved = loaded.save_state().unwrap();
    let mut reloaded =
        State::load_state(&resaved.bytes, Box::new(DefaultCallbacks), sealing_setup).unwrap();
    assert_eq!(support::global_string(&mut reloaded, "configured"), "nil");
}

#[test]
fn the_save_owns_every_non_string_global_key() {
    // Setup's non-string keys never merge: the saved table decides them.
    let setup = |state: &mut State| {
        support::run(state, "_G[1] = 'setup'; _G[true] = 'setup'");
        state.seal_globals();
        Ok::<(), std::convert::Infallible>(())
    };
    let mut state = State::new();
    setup(&mut state).unwrap();
    support::run(&mut state, "_G[1] = nil; _G[true] = nil; _G[2.5] = 'saved'");
    let save = state.save_state().unwrap();
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), setup).unwrap();
    support::run(
        &mut loaded,
        "if _G[1] ~= nil or _G[true] ~= nil or _G[2.5] ~= 'saved' then error('keys') end",
    );
    assert_eq!(loaded.save_state().unwrap().bytes, save.bytes);
}

#[test]
fn raw_globals_and_pairs_order_survive_a_load() {
    let mut state = State::new();
    support::run(
        &mut state,
        "rawset(_G, 'raw_one', 1); two = 2; rawset(_G, 'three', 3); two = nil; two = 22",
    );
    let order = |state: &mut State| {
        support::run(
            state,
            "local names = {} \
             for k in pairs(_G) do \
                 if k == 'raw_one' or k == 'two' or k == 'three' then names[#names + 1] = k end \
             end \
             order = table.concat(names, ',')",
        );
        support::global_string(state, "order")
    };
    let before = order(&mut state);
    assert_eq!(before, "raw_one,two,three");
    let save = state.save_state().unwrap();
    let mut loaded =
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup).unwrap();
    assert_eq!(order(&mut loaded), before);
    assert_eq!(support::global_string(&mut loaded, "three"), "3");
}

#[test]
fn load_restores_saved_global_order_over_setup_order() {
    // The saving State set `late` before `host_value`; setup installs only
    // `host_value`. The loaded order must follow the save, not setup.
    let mut state = State::new();
    support::run(&mut state, "late = 1");
    state.push_number(2.0).unwrap();
    state.set_global("host_value").unwrap();
    let save = state.save_state().unwrap();
    let loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        state.push_number(2.0)?;
        state.set_global("host_value")
    })
    .unwrap();
    assert_eq!(loaded.save_state().unwrap().bytes, save.bytes);
}

/// One queue step: push `count` values, pop all but `keep`, and summarize
/// what scripts can observe (`#`, `pairs` order, where an out-of-window write
/// lands).
const QUEUE_STEP: &str = r#"
    q = q or {}
    head = head or 1
    tail = tail or 0
    for _ = 1, count do
        tail = tail + 1
        q[tail] = tail
        if tail - head >= keep then
            q[head] = nil
            head = head + 1
        end
    end
    local keys = {}
    for k in pairs(q) do keys[#keys + 1] = tostring(k) end
    seen = #q .. ":" .. table.concat(keys, ",")
"#;

fn queue_step(state: &mut State, count: u32, keep: u32) -> String {
    state.push_number(f64::from(count)).unwrap();
    state.set_global("count").unwrap();
    state.push_number(f64::from(keep)).unwrap();
    state.set_global("keep").unwrap();
    support::run(state, QUEUE_STEP);
    support::global_string(state, "seen")
}

#[test]
fn queue_saves_stay_small_and_identical_across_load() {
    let mut state = State::new();
    queue_step(&mut state, 20, 3);
    let small = state.save_state().unwrap().bytes.len();
    queue_step(&mut state, 20_000, 3);
    let save = state.save_state().unwrap();
    // The dead history is not saved: a queue holding three values saves
    // about as much after twenty thousand pushes as after twenty.
    assert!(
        save.bytes.len() < small + 64,
        "queue save grew from {small} to {} bytes",
        save.bytes.len()
    );
    let mut loaded =
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup).unwrap();
    assert_eq!(loaded.save_state().unwrap().bytes, save.bytes);
    // Both continue identically, including a write far below the window
    // and the border it leaves.
    for current in [&mut state, &mut loaded] {
        support::run(current, "q[5] = 'low'");
    }
    for keep in [3, 0, 5] {
        assert_eq!(
            queue_step(&mut loaded, 50, keep),
            queue_step(&mut state, 50, keep)
        );
        assert_eq!(
            loaded.save_state().unwrap().bytes,
            state.save_state().unwrap().bytes
        );
    }
}
