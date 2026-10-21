//! `State::concat` reports a bad count as `InvalidStackIndex`, the kind `pop`
//! uses, not as an argument error about a Lua argument that does not exist.

use dellingr::State;
use dellingr::error::ErrorKind;

#[test]
fn concat_count_below_two_is_invalid_stack_index() {
    for n in [0usize, 1] {
        let mut state = State::new();
        state.push_number(1.0).unwrap();
        state.push_number(2.0).unwrap();
        let err = state.concat(n).unwrap_err();
        assert!(
            matches!(err.kind(), ErrorKind::InvalidStackIndex { index } if *index == -(n as isize)),
            "n={n}: {:?}",
            err.kind()
        );
        assert_eq!(state.get_top(), 2, "n={n}: stack must be unchanged");
    }
}

#[test]
fn concat_count_above_stack_is_invalid_stack_index() {
    let mut state = State::new();
    state.push_number(1.0).unwrap();
    let err = state.concat(2).unwrap_err();
    assert!(matches!(
        err.kind(),
        ErrorKind::InvalidStackIndex { index: -2 }
    ));
    assert_eq!(state.get_top(), 1);
}
