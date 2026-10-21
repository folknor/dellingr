#![cfg(feature = "snapshot")]

use dellingr::{ArgCount, DefaultCallbacks, RetCount, State};

fn run(state: &mut State, source: &str) {
    state.load_string(source).expect("test script compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("test script runs");
}

#[test]
fn builtin_table_alias_resolves_to_one_token_across_save_load() {
    let mut state = State::new();
    run(&mut state, "string = math");

    let save = state.save_state().expect("aliased builtin slots save");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {
        Ok::<(), std::convert::Infallible>(())
    })
    .expect("aliased builtin slots load");

    assert_eq!(
        loaded.save_state().expect("loaded state resaves").bytes,
        save.bytes
    );
    run(
        &mut loaded,
        "if math ~= string then error('builtin table alias was lost') end",
    );
}
