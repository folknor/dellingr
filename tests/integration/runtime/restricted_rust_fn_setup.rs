//! `State::set_global_named_rust_fn` is refused inside
//! `State::with_restricted_env`, before the id is registered, so the same id
//! installs normally once the environment is restored.

use dellingr::error::ErrorKind;
use dellingr::{LuaType, State};

fn host_fn(_state: &mut State) -> dellingr::Result<u8> {
    Ok(0)
}

#[test]
fn refused_named_registration_leaves_the_id_free_for_retry() {
    let mut state = State::new();
    state
        .with_restricted_env(&[], |state| {
            let error = state
                .set_global_named_rust_fn("host_setup", "host.setup.v1", host_fn)
                .expect_err("restricted setup must be refused");
            assert!(matches!(
                error.kind(),
                ErrorKind::SetGlobalNamedRustFnInRestrictedEnv
            ));
            assert_eq!(
                error.to_string(),
                "cannot register and install a named Rust function inside a restricted environment"
            );
            assert_eq!(state.get_top(), 0);
            assert!(state.registered_rust_fn("host.setup.v1").is_none());
            state
                .get_global("host_setup")
                .expect("global lookup succeeds");
            assert_eq!(state.typ(-1), LuaType::Nil);
            state.set_top(0).expect("the lookup result is removed");
        })
        .expect("restricted environment restores");

    state
        .set_global_named_rust_fn("host_setup", "host.setup.v1", host_fn)
        .expect("the rejected id remains available after restore");
    assert!(state.registered_rust_fn("host.setup.v1").is_some());
    state
        .get_global("host_setup")
        .expect("successful setup installs its global");
    assert_eq!(state.typ(-1), LuaType::Function);
}
