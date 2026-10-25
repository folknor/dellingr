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
        matches!(error, ErrorKind::ArgError(argument) if argument.detail() == &ArgDetail::NoInteger)
    );
    assert_eq!(
        ErrorKind::InvalidStackIndex { index: -1 }.to_string(),
        "invalid stack index (-1)",
    );
}

/// A second registration of an id is refused whichever function it names,
/// the same one included: registration never compares function pointers. The
/// original binding is untouched.
#[cfg(feature = "snapshot")]
#[test]
fn duplicate_registration_preserves_the_original_binding() {
    fn first(_state: &mut dellingr::State) -> dellingr::Result<u8> {
        Ok(0)
    }
    fn second(_state: &mut dellingr::State) -> dellingr::Result<u8> {
        Ok(1)
    }

    let mut state = dellingr::State::new();
    let original = state
        .register_rust_fn("host.same", first)
        .expect("first registration succeeds");
    let functions: [dellingr::RustFunc; 2] = [second, first];
    for function in functions {
        let error = state
            .register_rust_fn("host.same", function)
            .expect_err("id conflicts");
        assert!(matches!(
            error.kind(),
            ErrorKind::DuplicateFunctionRegistration { id } if id == "host.same"
        ));
        assert_eq!(
            error.kind().to_string(),
            "duplicate Rust function registration: host.same"
        );
    }
    assert_eq!(state.registered_rust_fn("host.same"), Some(original));
    let top = state.get_top();
    state
        .push_rust_fn(original)
        .expect("original binding remains");
    assert_eq!(state.get_top(), top + 1);
}
