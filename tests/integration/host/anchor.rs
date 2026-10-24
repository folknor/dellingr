//! Tests for the `Anchor` retainable-value API.

use dellingr::error::ErrorKind;
use dellingr::{Anchor, ArgCount, LuaType, RetCount, State};

use crate::support;

/// Anchor a fresh table and return the handle with the object count it
/// raised the heap to.
fn anchor_fresh_table(state: &mut State) -> (Anchor, usize) {
    support::eval(state, "return {}", 1);
    let a = state.anchor().unwrap();
    state.gc_collect();
    (a, state.object_count())
}

#[test]
fn anchor_round_trips_a_simple_value() {
    let mut state = State::new();
    state.push_number(42.0).unwrap();
    let a = state.anchor().unwrap();
    assert_eq!(state.anchor_count(), 1);
    state.push_anchor(&a).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 42.0);
}

#[test]
fn anchor_at_does_not_pop() {
    let mut state = State::new();
    state.push_number(7.0).unwrap();
    let _a = state.anchor_at(-1).unwrap();
    // Top is still 7.0.
    assert_eq!(state.to_number(-1).unwrap(), 7.0);
}

#[test]
fn anchor_count_tracks_inserts_releases_and_drops() {
    let mut state = State::new();

    state.push_number(1.0).unwrap();
    let a = state.anchor().unwrap();
    state.push_number(2.0).unwrap();
    let b = state.anchor().unwrap();
    state.push_number(3.0).unwrap();
    let c = state.anchor().unwrap();
    assert_eq!(state.anchor_count(), 3);

    assert!(state.release_anchor(b));
    assert_eq!(state.anchor_count(), 2);

    // A drop counts at once, before the State reclaims the slot.
    drop(a);
    assert_eq!(state.anchor_count(), 1);

    assert!(state.release_anchor(c));
    assert_eq!(state.anchor_count(), 0);
}

#[test]
fn dropped_anchor_is_collected_at_the_next_collection() {
    let mut state = State::new();
    state.gc_disable_auto();
    let (a, before) = anchor_fresh_table(&mut state);

    drop(a);
    state.gc_collect();
    assert_eq!(
        state.object_count(),
        before - 1,
        "GC should collect exactly the dropped anchor's table"
    );
}

#[test]
fn repeated_dropped_anchors_leave_no_live_handles() {
    let mut state = State::new();
    state.gc_disable_auto();
    for i in 0..1000 {
        state.push_number(f64::from(i)).unwrap();
        let a = state.anchor().unwrap();
        assert_eq!(state.anchor_count(), 1);
        state.push_anchor(&a).unwrap();
        assert_eq!(state.to_number(-1).unwrap(), f64::from(i));
        state.pop(1).unwrap();
        drop(a);
        assert_eq!(state.anchor_count(), 0);
    }
    // The public count observes live handles, not registry slots. Slot reuse
    // is covered by Registry's unit test.
    assert_eq!(state.anchor_count(), 0);
}

#[test]
fn released_anchor_does_not_release_twice() {
    let mut state = State::new();
    state.push_number(1.0).unwrap();
    let a = state.anchor().unwrap();
    state.push_number(2.0).unwrap();
    let b = state.anchor().unwrap();
    // `release_anchor` consumed `a`; its drop must not queue the removed
    // slot, which the next insert may hand to a new anchor.
    assert!(state.release_anchor(a));
    state.push_number(3.0).unwrap();
    let c = state.anchor().unwrap();
    state.gc_collect();
    assert_eq!(state.anchor_count(), 2);
    state.push_anchor(&b).unwrap();
    state.push_anchor(&c).unwrap();
    assert_eq!(state.to_number(-2).unwrap(), 2.0);
    assert_eq!(state.to_number(-1).unwrap(), 3.0);
}

#[test]
fn cross_state_anchor_is_rejected_and_released_where_issued() {
    let mut state_a = State::new();
    let mut state_b = State::new();

    state_a.push_number(123.0).unwrap();
    let a = state_a.anchor().unwrap();

    let err = state_b.push_anchor(&a).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidAnchor));
    assert_eq!(state_b.anchor_type(&a), None);

    // Releasing a wrong-State handle returns false and drops it, which
    // releases it in the State that issued it.
    assert!(!state_b.release_anchor(a));
    assert_eq!(state_a.anchor_count(), 0);
    state_a.gc_collect();
    assert_eq!(state_a.anchor_count(), 0);
}

#[test]
fn anchor_dropped_on_another_thread_is_released() {
    let mut state = State::new();
    state.gc_disable_auto();
    let (a, before) = anchor_fresh_table(&mut state);
    std::thread::spawn(move || drop(a))
        .join()
        .expect("drop thread does not panic");
    assert_eq!(state.anchor_count(), 0);
    state.gc_collect();
    assert_eq!(state.object_count(), before - 1);
}

#[test]
fn callback_dropping_an_anchor_mid_execution_is_collected_safely() {
    // A Rust callback anchors a fresh table, drops the handle and collects
    // while the calling Lua frame is live; the frame's own values survive.
    fn churn(state: &mut State) -> dellingr::Result<u8> {
        state.new_table()?;
        let a = state.anchor()?;
        drop(a);
        state.gc_collect();
        Ok(0)
    }
    let mut state = State::new();
    state.push_rust_fn(churn).unwrap();
    state.set_global("churn").unwrap();
    support::run(
        &mut state,
        "local kept = { v = 5 }
             for _ = 1, 10 do churn() end
             result = kept.v",
    );
    state.get_global("result").unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 5.0);
    assert_eq!(state.anchor_count(), 0);
}

#[test]
fn anchor_may_outlive_its_state() {
    let mut state = State::new();
    state.push_number(1.0).unwrap();
    let a = state.anchor().unwrap();
    drop(state);
    drop(a);
}

#[test]
fn anchor_nil_is_rejected() {
    let mut state = State::new();
    state.push_nil().unwrap();
    let err = state.anchor().unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::AnchorNil));
}

#[test]
fn anchor_at_nil_is_rejected_without_popping() {
    let mut state = State::new();
    state.push_number(1.0).unwrap();
    state.push_nil().unwrap();

    let err = state.anchor_at(-1).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::AnchorNil));

    // Stack still holds [1.0, nil]; anchor_at didn't touch it.
    assert_eq!(state.typ(-1), LuaType::Nil);
    assert_eq!(state.to_number(-2).unwrap(), 1.0);
}

#[test]
fn anchor_function_accepts_lua_function() {
    let mut state = State::new();
    support::eval(&mut state, "return function(x) return x + 1 end", 1);
    let a = state.anchor_function().unwrap();

    state.push_number(10.0).unwrap();
    state
        .call_anchor(&a, ArgCount::Fixed(1), RetCount::Fixed(1))
        .unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 11.0);
}

#[test]
fn anchor_function_accepts_rust_function() {
    let mut state = State::new();
    state
        .push_rust_fn(|state| {
            state.push_number(7.0).unwrap();
            Ok(1)
        })
        .unwrap();
    let a = state.anchor_function().unwrap();
    state
        .call_anchor(&a, ArgCount::Fixed(0), RetCount::Fixed(1))
        .unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 7.0);
}

#[test]
fn anchor_function_rejects_number() {
    let mut state = State::new();
    state.push_number(1.0).unwrap();
    assert!(state.anchor_function().is_err());
}

#[test]
fn anchor_does_not_pop_on_nil_error() {
    let mut state = State::new();
    state.push_number(7.0).unwrap();
    state.push_nil().unwrap();
    assert!(matches!(
        state.anchor().unwrap_err().kind(),
        ErrorKind::AnchorNil,
    ));
    // Error path leaves the stack untouched, matching anchor_at.
    assert_eq!(state.get_top(), 2);
    state.pop(1).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 7.0);
}

#[test]
fn anchor_function_does_not_pop_on_type_error() {
    let mut state = State::new();
    state.push_number(7.0).unwrap();
    state.push_number(1.0).unwrap();
    let err = state.anchor_function().unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::TypeError(_)));
    assert_eq!(state.get_top(), 2);
    assert_eq!(state.to_number(-1).unwrap(), 1.0);
    assert_eq!(state.to_number(-2).unwrap(), 7.0);
}

#[test]
fn call_anchor_rejects_dynamic_arg_count() {
    let mut state = State::new();
    support::eval(&mut state, "return function() return 1 end", 1);
    let a = state.anchor_function().unwrap();
    let err = state
        .call_anchor(&a, ArgCount::Dynamic, RetCount::Fixed(0))
        .expect_err("Dynamic args have no host-API call-base, must be rejected");
    assert!(matches!(
        err.kind(),
        ErrorKind::DynamicArgumentsUnsupported {
            api: dellingr::error::HostCallApi::CallAnchor
        }
    ));
}

#[test]
fn anchored_closure_survives_gc_after_global_dropped() {
    let mut state = State::new();
    state.gc_disable_auto();

    // Build a closure that returns a captured value, push it as a global.
    support::run(
        &mut state,
        r#"
            captured = 7
            handler = function() return captured end
        "#,
    );

    // Anchor the global so we can safely drop it.
    state.get_global("handler").unwrap();
    let h = state.anchor_function().unwrap();

    // Drop the global reference and force a GC.
    state.push_nil().unwrap();
    state.set_global("handler").unwrap();
    state.gc_collect();

    // The closure should still be reachable through the anchor.
    state
        .call_anchor(&h, ArgCount::Fixed(0), RetCount::Fixed(1))
        .unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 7.0);
}

#[test]
fn release_then_gc_collects_anchored_object() {
    let mut state = State::new();
    state.gc_disable_auto();
    let (a, before) = anchor_fresh_table(&mut state);

    // Release the anchor; the table is now unreachable.
    assert!(state.release_anchor(a));
    state.gc_collect();
    let after = state.object_count();
    assert_eq!(
        after,
        before - 1,
        "GC should collect exactly the released table"
    );
}

#[test]
fn anchor_invisible_to_with_restricted_env() {
    let mut state = State::new();

    // Anchor a value; it's not in globals.
    state.push_number(1234.0).unwrap();
    let a = state.anchor().unwrap();

    // Run a restricted-env block; the anchor is still valid inside.
    state
        .with_restricted_env(&[], |state| {
            state.push_anchor(&a).unwrap();
            assert_eq!(state.to_number(-1).unwrap(), 1234.0);
            state.pop(1).unwrap();
        })
        .expect("state is not poisoned");

    // And still valid outside.
    state.push_anchor(&a).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 1234.0);
}

#[test]
fn anchor_type_reflects_underlying_value() {
    let mut state = State::new();

    state.push_number(1.0).unwrap();
    let a_num = state.anchor().unwrap();
    assert_eq!(state.anchor_type(&a_num), Some(LuaType::Number));

    state.push_string("hello").expect("short test string fits");
    let a_str = state.anchor().unwrap();
    assert_eq!(state.anchor_type(&a_str), Some(LuaType::String));

    support::eval(&mut state, "return function() end", 1);
    let a_fn = state.anchor().unwrap();
    assert_eq!(state.anchor_type(&a_fn), Some(LuaType::Function));
}

#[test]
fn anchor_arithmetic_outputs_match_between_two_states() {
    fn run(state: &mut State) -> String {
        state.gc_disable_auto();

        // Anchor two functions, exercise the slotmap.
        support::eval(state, "return function(x) return x * 2 end", 1);
        let f = state.anchor_function().unwrap();

        support::eval(state, "return function(x) return x + 100 end", 1);
        let g = state.anchor_function().unwrap();

        // Use them, release, anchor again to provoke slot reuse.
        state.push_number(3.0).unwrap();
        state
            .call_anchor(&f, ArgCount::Fixed(1), RetCount::Fixed(1))
            .unwrap();
        let r1 = state.to_number(-1).unwrap();
        state.pop(1).unwrap();

        state.push_number(5.0).unwrap();
        state
            .call_anchor(&g, ArgCount::Fixed(1), RetCount::Fixed(1))
            .unwrap();
        let r2 = state.to_number(-1).unwrap();
        state.pop(1).unwrap();

        assert!(state.release_anchor(f));
        drop(g);

        support::eval(state, "return function(x) return x - 1 end", 1);
        let h = state.anchor_function().unwrap();
        state.push_number(10.0).unwrap();
        state
            .call_anchor(&h, ArgCount::Fixed(1), RetCount::Fixed(1))
            .unwrap();
        let r3 = state.to_number(-1).unwrap();
        state.pop(1).unwrap();

        format!("{r1},{r2},{r3}")
    }

    let mut a = State::new();
    let mut b = State::new();
    assert_eq!(run(&mut a), run(&mut b));
}
