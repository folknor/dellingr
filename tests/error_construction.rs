//! Structured host errors retain their category and diagnostic data.

use dellingr::error::{ArgDetail, ErrorKind};

#[test]
fn typed_argument_detail_preserves_variant_and_rendering() {
    let error = ErrorKind::argument_detail(2, "host", ArgDetail::NoInteger);
    assert_eq!(
        error.to_string(),
        "bad argument #2 to 'host' (number has no integer representation)"
    );
    assert!(
        matches!(error, ErrorKind::ArgError(argument) if argument.detail == ArgDetail::NoInteger)
    );
    assert_eq!(
        ErrorKind::InvalidStackIndex { index: -1 }.to_string(),
        "invalid stack index (-1)",
    );
}

#[cfg(feature = "snapshot")]
#[test]
fn duplicate_named_push_preserves_id_and_stack() {
    fn first(_state: &mut dellingr::State) -> dellingr::Result<u8> {
        Ok(0)
    }
    fn second(_state: &mut dellingr::State) -> dellingr::Result<u8> {
        Ok(1)
    }

    let mut state = dellingr::State::new();
    state
        .register_rust_fn("host.same", first)
        .expect("first registration succeeds");
    let top = state.get_top();
    let error = state
        .push_named_rust_fn("host.same", second)
        .expect_err("id conflicts");
    assert!(
        matches!(error.kind, ErrorKind::DuplicateFunctionRegistration { id } if id == "host.same")
    );
    assert_eq!(state.get_top(), top);
    state
        .push_named_rust_fn("host.same", first)
        .expect("original binding remains");
}
