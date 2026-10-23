//! `State::open_libs` is refused inside `State::with_restricted_env`, before
//! any state changes, and works again once the environment is restored.

use dellingr::{ArgCount, Program, RetCount, State};

#[test]
fn restricted_library_setup_is_refused_without_mutation() {
    let mut state = State::new();
    state.gc_disable_auto();
    let heap_size = state.heap_size();
    let top = state.get_top();
    state
        .with_restricted_env(&[], |state| {
            let error = state.open_libs().expect_err("restricted library setup");
            assert!(matches!(
                error.kind(),
                dellingr::error::ErrorKind::OpenLibsInRestrictedEnv
            ));
            assert_eq!(
                error.kind().to_string(),
                "cannot open libraries inside a restricted environment"
            );
            assert_eq!(state.heap_size(), heap_size);
            assert_eq!(state.get_top(), top);
            state
                .with_restricted_env(&[], |state| {
                    assert!(state.open_libs().is_err());
                })
                .unwrap();
        })
        .unwrap();
    let program = Program::compile(
        "if ('abc'):upper() ~= string.upper('abc') then error('string library changed') end",
        None,
    )
    .unwrap();
    state.load(&program).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
    state
        .open_libs()
        .expect("restored environment allows setup");
}

#[cfg(feature = "snapshot")]
#[test]
fn refused_restricted_setup_preserves_snapshot_bytes() {
    let mut state = State::new();
    let before = state.save_state().unwrap();
    state
        .with_restricted_env(&[], |state| {
            assert!(state.open_libs().is_err());
        })
        .unwrap();
    assert_eq!(state.save_state().unwrap().bytes, before.bytes);
}
