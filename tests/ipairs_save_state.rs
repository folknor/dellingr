#![cfg(feature = "snapshot")]

use std::convert::Infallible;

use dellingr::{ArgCount, DefaultCallbacks, RetCount, State};

#[test]
fn ipairs_iterator_round_trips_through_save_state() {
    let mut state = State::new();
    dellingr::Program::compile("iter, iter_state, iter_key = ipairs({ 7 })", None)
        .and_then(|program| state.load(&program))
        .expect("iterator setup compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("iterator setup runs");

    let save = state
        .save_state()
        .expect("state with ipairs iterator saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {
        Ok::<(), Infallible>(())
    })
    .expect("state with ipairs iterator loads");

    dellingr::Program::compile(
        r#"for index, value in iter, iter_state, iter_key do result = index + value end
               if result ~= 8 then error('ipairs iterator did not resume') end"#,
        None,
    )
    .and_then(|program| loaded.load(&program))
    .expect("iterator continuation compiles");
    loaded
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("loaded ipairs iterator returns the saved table value");
}
