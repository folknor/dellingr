//! A warm global-lookup cache must not leak a global into, or out of, a
//! restricted environment.

use dellingr::{ArgCount, RetCount, State};

use crate::support;

#[test]
fn global_lookup_cache_respects_restricted_env() {
    let mut state = State::new();
    support::run(
        &mut state,
        r#"
            x = 1
            function read_x()
                if x == nil then return 2 end
                return x
            end
        "#,
    );

    state.get_global("read_x").unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 1.0);
    state.pop(1).unwrap();

    let restricted = state
        .with_restricted_env(&["read_x"], |state| {
            state.get_global("read_x").unwrap();
            state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
            let result = state.to_number(-1).unwrap();
            state.pop(1).unwrap();
            result
        })
        .expect("state is not poisoned");
    assert_eq!(restricted, 2.0);

    state.get_global("read_x").unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 1.0);
}
