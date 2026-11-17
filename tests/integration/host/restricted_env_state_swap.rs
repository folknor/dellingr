//! A `State::with_restricted_env` closure (and a snapshot loader's setup)
//! holds `&mut State`, so it can move the State out with `mem::take` or
//! `mem::swap`. The boundary refuses with a typed error instead of popping
//! the replacement's suspension stack (which used to panic) or resuming a
//! suspension another level owns.

use dellingr::error::ErrorKind;
use dellingr::{LuaType, State};

fn global_type(state: &mut State, name: &str) -> LuaType {
    state.get_global(name).expect("global lookup succeeds");
    let typ = state.typ(-1);
    state.set_top(0).expect("the lookup result is removed");
    typ
}

fn assert_replaced<R: std::fmt::Debug>(result: dellingr::Result<R>) {
    let error = result.expect_err("a moved-out State must be refused");
    assert!(
        matches!(error.kind(), ErrorKind::StateReplacedInRestrictedEnv),
        "{error}"
    );
}

#[test]
fn taking_the_state_out_is_refused_and_leaves_both_states_coherent() {
    let mut state = State::new();
    let mut escaped = None;
    let result = state.with_restricted_env(&[], |state| {
        escaped = Some(std::mem::take(state));
    });
    assert_replaced(result);

    // The replacement in the slot was never restricted and is not poisoned.
    assert!(!state.is_poisoned());
    assert_eq!(global_type(&mut state, "print"), LuaType::Function);

    // The moved-out original keeps the restricted environment.
    let mut escaped = escaped.expect("the closure ran");
    assert!(!escaped.is_poisoned());
    assert_eq!(global_type(&mut escaped, "print"), LuaType::Nil);
}

#[test]
fn putting_the_state_back_before_returning_restores_normally() {
    let mut state = State::new();
    let value = state
        .with_restricted_env(&[], |state| {
            let original = std::mem::take(state);
            *state = original;
            7
        })
        .expect("the restricted State is back in the slot");
    assert_eq!(value, 7);
    assert_eq!(global_type(&mut state, "print"), LuaType::Function);
}

#[test]
fn swapping_with_another_state_leaves_the_other_untouched() {
    let mut state = State::new();
    let mut other = State::new();
    other.push_number(1.0).expect("other is usable");
    other.set_global("marker").expect("other is usable");
    let result = state.with_restricted_env(&[], |state| std::mem::swap(state, &mut other));
    assert_replaced(result);

    // The other State, now in the slot, kept its full environment.
    assert_eq!(global_type(&mut state, "marker"), LuaType::Number);
    assert_eq!(global_type(&mut state, "print"), LuaType::Function);
    // The original, now in `other`, stays restricted.
    assert_eq!(global_type(&mut other, "print"), LuaType::Nil);
}

#[test]
fn an_outer_level_does_not_pop_an_abandoned_inner_suspension() {
    let mut state = State::new();
    let outer = state.with_restricted_env(&["print"], |state| {
        let mut escaped = None;
        let inner = state.with_restricted_env(&[], |state| {
            escaped = Some(std::mem::take(state));
        });
        assert_replaced(inner);
        // Put the original back: its inner suspension is still on top.
        *state = escaped.expect("the inner closure ran");
    });
    assert_replaced(outer);

    // Popping the inner entry would have resumed the outer restricted
    // environment and reported success; both suspensions stay instead.
    assert!(!state.is_poisoned());
    assert_eq!(global_type(&mut state, "print"), LuaType::Nil);
}

#[cfg(feature = "snapshot")]
#[test]
fn load_setup_that_moves_the_state_out_is_refused() {
    let save = State::new().save_state().expect("a fresh State saves");
    let result = State::load_state(
        &save.bytes,
        Box::new(dellingr::DefaultCallbacks),
        |state| -> dellingr::Result<()> {
            drop(std::mem::take(state));
            Ok(())
        },
    );
    assert!(matches!(
        result,
        Err(dellingr::LoadError::StateReplacedInSetup)
    ));
}

#[cfg(feature = "snapshot")]
#[test]
fn load_setup_that_leaves_a_restriction_suspended_is_refused() {
    let save = State::new().save_state().expect("a fresh State saves");
    let result = State::load_state(
        &save.bytes,
        Box::new(dellingr::DefaultCallbacks),
        |state| -> dellingr::Result<()> {
            let mut escaped = None;
            let refused = state.with_restricted_env(&[], |state| {
                escaped = Some(std::mem::take(state));
            });
            assert_replaced(refused);
            *state = escaped.expect("the closure ran");
            Ok(())
        },
    );
    assert!(matches!(
        result,
        Err(dellingr::LoadError::StateReplacedInSetup)
    ));
}
