//! Tests for the `Anchor` retainable-value API.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, LuaType, RetCount, State};

fn load_function(state: &mut State, src: &str) {
    state.load_string(src).unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("function expression should produce a value");
}

#[test]
fn anchor_round_trips_a_simple_value() {
    let mut state = State::new();
    state.push_number(42.0);
    let a = state.anchor().unwrap();
    assert_eq!(state.anchor_count(), 1);
    state.push_anchor(a).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 42.0);
}

#[test]
fn anchor_at_does_not_pop() {
    let mut state = State::new();
    state.push_number(7.0);
    let _a = state.anchor_at(-1).unwrap();
    // Top is still 7.0.
    assert_eq!(state.to_number(-1).unwrap(), 7.0);
}

#[test]
fn anchor_count_tracks_inserts_and_releases() {
    let mut state = State::new();

    state.push_number(1.0);
    let a = state.anchor().unwrap();
    state.push_number(2.0);
    let b = state.anchor().unwrap();
    state.push_number(3.0);
    let c = state.anchor().unwrap();
    assert_eq!(state.anchor_count(), 3);

    assert!(state.release_anchor(b));
    assert_eq!(state.anchor_count(), 2);

    // Idempotent / stale release returns false.
    assert!(!state.release_anchor(b));
    assert_eq!(state.anchor_count(), 2);

    assert!(state.release_anchor(a));
    assert!(state.release_anchor(c));
    assert_eq!(state.anchor_count(), 0);
}

#[test]
fn push_anchor_after_release_returns_invalid_anchor() {
    let mut state = State::new();
    state.push_number(99.0);
    let a = state.anchor().unwrap();
    assert!(state.release_anchor(a));

    let err = state.push_anchor(a).unwrap_err();
    assert!(matches!(err.kind, ErrorKind::InvalidAnchor));
}

#[test]
fn generation_rejects_stale_handle_after_slot_reuse() {
    let mut state = State::new();

    state.push_number(1.0);
    let a = state.anchor().unwrap();
    assert!(state.release_anchor(a));

    // The next anchor likely lands in the same slot; generation should differ.
    state.push_number(2.0);
    let b = state.anchor().unwrap();
    assert_ne!(a, b, "slotmap should bump the generation on reuse");

    // Old handle is now stale.
    let err = state.push_anchor(a).unwrap_err();
    assert!(matches!(err.kind, ErrorKind::InvalidAnchor));

    // New handle still works.
    state.push_anchor(b).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 2.0);
}

#[test]
fn cross_state_anchor_is_rejected() {
    let mut state_a = State::new();
    let mut state_b = State::new();

    state_a.push_number(123.0);
    let a = state_a.anchor().unwrap();

    let err = state_b.push_anchor(a).unwrap_err();
    assert!(matches!(err.kind, ErrorKind::InvalidAnchor));

    // Releasing a wrong-state handle is a no-op (returns false).
    assert!(!state_b.release_anchor(a));
}

#[test]
fn anchor_nil_is_rejected() {
    let mut state = State::new();
    state.push_nil();
    let err = state.anchor().unwrap_err();
    assert!(matches!(err.kind, ErrorKind::AnchorNil));
}

#[test]
fn anchor_at_nil_is_rejected_without_popping() {
    let mut state = State::new();
    state.push_number(1.0);
    state.push_nil();

    let err = state.anchor_at(-1).unwrap_err();
    assert!(matches!(err.kind, ErrorKind::AnchorNil));

    // Stack still holds [1.0, nil]; anchor_at didn't touch it.
    assert_eq!(state.typ(-1), LuaType::Nil);
    assert_eq!(state.to_number(-2).unwrap(), 1.0);
}

#[test]
fn anchor_function_accepts_lua_function() {
    let mut state = State::new();
    load_function(&mut state, "return function(x) return x + 1 end");
    let a = state.anchor_function().unwrap();

    state.push_number(10.0);
    state
        .call_anchor(a, ArgCount::Fixed(1), RetCount::Fixed(1))
        .unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 11.0);
}

#[test]
fn anchor_function_accepts_rust_function() {
    let mut state = State::new();
    state.push_rust_fn(|state| {
        state.push_number(7.0);
        Ok(1)
    });
    let a = state.anchor_function().unwrap();
    state
        .call_anchor(a, ArgCount::Fixed(0), RetCount::Fixed(1))
        .unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 7.0);
}

#[test]
fn anchor_function_rejects_number() {
    let mut state = State::new();
    state.push_number(1.0);
    assert!(state.anchor_function().is_err());
}

#[test]
fn anchor_does_not_pop_on_nil_error() {
    let mut state = State::new();
    state.push_number(7.0);
    state.push_nil();
    assert!(matches!(
        state.anchor().unwrap_err().kind,
        ErrorKind::AnchorNil,
    ));
    // Error path leaves the stack untouched, matching anchor_at.
    assert_eq!(state.get_top(), 2);
    state.pop(1);
    assert_eq!(state.to_number(-1).unwrap(), 7.0);
}

#[test]
fn anchor_function_does_not_pop_on_type_error() {
    let mut state = State::new();
    state.push_number(7.0);
    state.push_number(1.0);
    let err = state.anchor_function().unwrap_err();
    assert!(matches!(err.kind, ErrorKind::TypeError(_)));
    assert_eq!(state.get_top(), 2);
    assert_eq!(state.to_number(-1).unwrap(), 1.0);
    assert_eq!(state.to_number(-2).unwrap(), 7.0);
}

#[test]
fn call_anchor_rejects_dynamic_arg_count() {
    let mut state = State::new();
    load_function(&mut state, "return function() return 1 end");
    let a = state.anchor_function().unwrap();
    let err = state
        .call_anchor(a, ArgCount::Dynamic, RetCount::Fixed(0))
        .expect_err("Dynamic args have no host-API call-base, must be rejected");
    assert!(matches!(err.kind, ErrorKind::InternalError(_)));
}

#[test]
fn anchored_closure_survives_gc_after_global_dropped() {
    let mut state = State::new();
    state.gc_disable_auto();

    // Build a closure that returns a captured value, push it as a global.
    state
        .load_string(
            r#"
            captured = 7
            handler = function() return captured end
        "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    // Anchor the global so we can safely drop it.
    state.get_global("handler");
    let h = state.anchor_function().unwrap();

    // Drop the global reference and force a GC.
    state.push_nil();
    state.set_global("handler");
    state.gc_collect();

    // The closure should still be reachable through the anchor.
    state
        .call_anchor(h, ArgCount::Fixed(0), RetCount::Fixed(1))
        .unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 7.0);
}

#[test]
fn release_then_gc_collects_anchored_object() {
    let mut state = State::new();
    state.gc_disable_auto();

    // Make a fresh table, anchor it, and forget it from globals.
    state.load_string("return {}").unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    let a = state.anchor().unwrap();

    let before = state.object_count();
    assert!(before >= 1);

    // Release the anchor; the table is now unreachable.
    assert!(state.release_anchor(a));
    state.gc_collect();
    let after = state.object_count();
    assert!(
        after < before,
        "expected GC to collect the released table (before={before}, after={after})"
    );
}

#[test]
fn anchor_invisible_to_with_restricted_env() {
    let mut state = State::new();

    // Anchor a value; it's not in globals.
    state.push_number(1234.0);
    let a = state.anchor().unwrap();

    // Run a restricted-env block; the anchor is still valid inside.
    state.with_restricted_env(&[], |state| {
        state.push_anchor(a).unwrap();
        assert_eq!(state.to_number(-1).unwrap(), 1234.0);
        state.pop(1);
    });

    // And still valid outside.
    state.push_anchor(a).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 1234.0);
}

#[test]
fn anchor_type_reflects_underlying_value() {
    let mut state = State::new();

    state.push_number(1.0);
    let a_num = state.anchor().unwrap();
    assert_eq!(state.anchor_type(a_num), Some(LuaType::Number));

    state.push_string("hello");
    let a_str = state.anchor().unwrap();
    assert_eq!(state.anchor_type(a_str), Some(LuaType::String));

    load_function(&mut state, "return function() end");
    let a_fn = state.anchor().unwrap();
    assert_eq!(state.anchor_type(a_fn), Some(LuaType::Function));

    state.release_anchor(a_num);
    assert_eq!(state.anchor_type(a_num), None);
}

#[test]
fn determinism_two_states_same_sequence_same_output() {
    fn run(state: &mut State) -> String {
        state.gc_disable_auto();

        // Anchor two functions, exercise the slotmap.
        load_function(state, "return function(x) return x * 2 end");
        let f = state.anchor_function().unwrap();

        load_function(state, "return function(x) return x + 100 end");
        let g = state.anchor_function().unwrap();

        // Use them, release, anchor again to provoke slot reuse.
        state.push_number(3.0);
        state
            .call_anchor(f, ArgCount::Fixed(1), RetCount::Fixed(1))
            .unwrap();
        let r1 = state.to_number(-1).unwrap();
        state.pop(1);

        state.push_number(5.0);
        state
            .call_anchor(g, ArgCount::Fixed(1), RetCount::Fixed(1))
            .unwrap();
        let r2 = state.to_number(-1).unwrap();
        state.pop(1);

        assert!(state.release_anchor(f));
        assert!(state.release_anchor(g));

        load_function(state, "return function(x) return x - 1 end");
        let h = state.anchor_function().unwrap();
        state.push_number(10.0);
        state
            .call_anchor(h, ArgCount::Fixed(1), RetCount::Fixed(1))
            .unwrap();
        let r3 = state.to_number(-1).unwrap();
        state.pop(1);

        format!("{r1},{r2},{r3}")
    }

    let mut a = State::new();
    let mut b = State::new();
    assert_eq!(run(&mut a), run(&mut b));
}
