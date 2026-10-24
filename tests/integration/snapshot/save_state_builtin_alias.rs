use dellingr::{DefaultCallbacks, State};

use crate::support;

#[test]
fn builtin_table_alias_resolves_to_one_token_across_save_load() {
    let mut state = State::new();
    support::run(&mut state, "string = math");

    let save = state.save_state().expect("aliased builtin slots save");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup)
        .expect("aliased builtin slots load");

    assert_eq!(
        loaded.save_state().expect("loaded state resaves").bytes,
        save.bytes
    );
    support::run(
        &mut loaded,
        "if math ~= string then error('builtin table alias was lost') end",
    );
}
