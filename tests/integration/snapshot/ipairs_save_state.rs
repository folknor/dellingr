use dellingr::{DefaultCallbacks, State};

use crate::support;

#[test]
fn ipairs_iterator_round_trips_through_save_state() {
    let mut state = State::new();
    support::run(&mut state, "iter, iter_state, iter_key = ipairs({ 7 })");

    let save = state
        .save_state()
        .expect("state with ipairs iterator saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup)
        .expect("state with ipairs iterator loads");

    support::run(
        &mut loaded,
        r#"for index, value in iter, iter_state, iter_key do result = index + value end
               if result ~= 8 then error('ipairs iterator did not resume') end"#,
    );
}
