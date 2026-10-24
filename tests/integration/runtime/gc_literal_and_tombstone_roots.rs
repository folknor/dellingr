//! GC roots that are not closures: tombstoned table slots, string literals
//! of an active frame, and literals of a swept runtime bundle.

use dellingr::State;

use crate::support;
use crate::support::PushNewRustFn;

#[test]
fn tombstoned_keys_and_values_are_not_gc_roots() {
    // Dead slots retain their keys but never their values. Assignments keep
    // the empty case inline. Eight fractional keys force map storage and keep
    // compaction from removing the tombstones below.
    for preset in [0, 8] {
        let mut state = State::new();
        state.gc_disable_auto();
        support::run(
            &mut state,
            format!("t = {{}}; for i = 1, {preset} do t[i + 0.5] = i end"),
        );
        state.gc_collect();
        let baseline = state.object_count();

        // The map case leaves both a Vacant slot and a Live(key) tombstone.
        support::run(
            &mut state,
            "local key = {}; local value = {}; t[key] = value; t[key] = nil; t[key] = value; t[key] = nil",
        );
        state.gc_collect();
        // A dead slot keeps its key as a strong reference until compaction, so
        // a cleared traversal control stays valid; the cleared value is not
        // retained.
        assert_eq!(state.object_count(), baseline + 1, "preset {preset}");

        // Traverse and then force compaction with collected keys still in the
        // dead slots. Neither path may dereference or expose those keys.
        support::run(
            &mut state,
            format!(
                "local count = 0; for k in pairs(t) do count = count + 1; t[k] = nil end; if count ~= {preset} then error('dead key escaped') end; t[0.5] = true",
            ),
        );
    }
}

#[test]
fn active_frame_string_literal_survives_explicit_gc() {
    let mut state = State::new();
    state.gc_disable_auto();
    install_force_gc(&mut state);

    support::eval(
        &mut state,
        r#"
        force_gc()
        return "literal after gc"
    "#,
        1,
    );

    assert_eq!(state.to_string(-1).unwrap(), "literal after gc");
    state.pop(1).unwrap();
}

#[test]
fn string_literals_are_reclaimed_one_collection_after_runtime_cache_sweep() {
    let mut state = State::raw();
    state.gc_disable_auto();

    // A fresh State already holds its permanent interned strings.
    let baseline = state.string_count();
    support::run(
        &mut state,
        r#"
        local temporary = "collect this literal after return"
    "#,
    );

    assert!(state.string_count() > baseline);
    state.gc_collect();
    // The sweep drops the dead runtime bundle after root marking, so its
    // literals intentionally survive this collection and disappear on the
    // next one.
    assert!(state.string_count() > baseline);
    state.gc_collect();
    assert_eq!(state.string_count(), baseline);
}

fn install_force_gc(state: &mut State) {
    state
        .push_new_rust_fn(|state| {
            state.gc_collect();
            Ok(0)
        })
        .unwrap();
    state.set_global("force_gc").unwrap();
}
