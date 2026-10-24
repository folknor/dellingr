//! Compile, load and call helpers. Each panics with the source and the error
//! when a step the caller did not ask to fail fails, so a test states only
//! what it checks.

use dellingr::error::Error;
use dellingr::{ArgCount, RetCount, State};

/// Test shorthand for a Rust function a test pushes once or a few times:
/// create its entry and push it. Each call creates a new function identity
/// (see `State::add_rust_fn`); a test that compares identities keeps the
/// handle instead.
pub(crate) trait PushNewRustFn {
    fn push_new_rust_fn(&mut self, function: dellingr::RustFunc) -> dellingr::Result<()>;
}

impl PushNewRustFn for State {
    fn push_new_rust_fn(&mut self, function: dellingr::RustFunc) -> dellingr::Result<()> {
        let handle = self.add_rust_fn(function)?;
        self.push_rust_fn(handle)
    }
}

/// Compile `source` as an unnamed chunk and push it onto `state`'s stack.
pub(crate) fn load(state: &mut State, source: impl AsRef<[u8]>) {
    let source = source.as_ref();
    if let Err(error) =
        dellingr::Program::compile(source, None).and_then(|program| state.load(&program))
    {
        panic!(
            "test source must compile and load:\n{}\n{error}",
            String::from_utf8_lossy(source)
        );
    }
}

/// Load `source` and call it with no arguments, keeping `results` of its
/// results on the stack.
pub(crate) fn eval(state: &mut State, source: impl AsRef<[u8]>, results: u8) {
    let source = source.as_ref();
    load(state, source);
    if let Err(error) = state.call(ArgCount::Fixed(0), RetCount::Fixed(results)) {
        panic!(
            "test source must run:\n{}\n{error}",
            String::from_utf8_lossy(source)
        );
    }
}

/// Load and run `source` on `state`, discarding its results.
pub(crate) fn run(state: &mut State, source: impl AsRef<[u8]>) {
    eval(state, source, 0);
}

/// Compile, load and run `source` on `state`, returning the first error any
/// step raises.
pub(crate) fn try_run(state: &mut State, source: impl AsRef<[u8]>) -> dellingr::Result<()> {
    dellingr::Program::compile(source, None).and_then(|program| state.load(&program))?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
}

/// The runtime error `source` raises on a fresh standard State. It must
/// compile.
pub(crate) fn run_error(source: impl AsRef<[u8]>) -> Error {
    let source = source.as_ref();
    let mut state = State::new();
    load(&mut state, source);
    match state.call(ArgCount::Fixed(0), RetCount::Fixed(0)) {
        Ok(()) => panic!(
            "test source must fail:\n{}",
            String::from_utf8_lossy(source)
        ),
        Err(error) => error,
    }
}

/// The number `source` returns on a fresh standard State.
pub(crate) fn eval_number(source: impl AsRef<[u8]>) -> f64 {
    let mut state = State::new();
    eval(&mut state, source, 1);
    state
        .to_number(-1)
        .expect("test source must return a number")
}

/// The truthiness of the value `source` returns on a fresh standard State.
pub(crate) fn eval_boolean(source: impl AsRef<[u8]>) -> bool {
    let mut state = State::new();
    eval(&mut state, source, 1);
    state.to_boolean(-1)
}

/// The string bytes `source` returns on a fresh standard State.
pub(crate) fn eval_bytes(source: impl AsRef<[u8]>) -> Vec<u8> {
    let mut state = State::new();
    eval(&mut state, source, 1);
    state
        .to_bytes(-1)
        .expect("test source must return a string")
        .to_vec()
}

/// What running `source` on a fresh standard State charges.
pub(crate) fn cost_of(source: impl AsRef<[u8]>) -> u64 {
    let mut state = State::new();
    run(&mut state, source);
    state.cost_used()
}

/// Global `name` as a number, leaving the stack as it was.
pub(crate) fn global_number(state: &mut State, name: &str) -> f64 {
    state.get_global(name).expect("the global is readable");
    let value = state.to_number(-1).expect("the global is a number");
    state.pop(1).expect("the global was pushed");
    value
}

/// A `State::load_state` setup that installs nothing.
#[cfg(feature = "snapshot")]
pub(crate) fn no_setup(_: &mut State) -> Result<(), std::convert::Infallible> {
    Ok(())
}

/// `state` saved and loaded back into a fresh State with default callbacks.
#[cfg(feature = "snapshot")]
pub(crate) fn reload(state: &State) -> State {
    let save = state.save_state().expect("the State saves");
    State::load_state(&save.bytes, Box::new(dellingr::DefaultCallbacks), no_setup)
        .expect("the save loads")
}

/// Global `name` rendered as `tostring` renders it, leaving the stack as it
/// was. Only the snapshot suites read globals back this way.
#[cfg(feature = "snapshot")]
pub(crate) fn global_string(state: &mut State, name: &str) -> String {
    state.get_global(name).expect("the global is readable");
    let value = state.to_string_with_meta(-1).expect("the global renders");
    state.pop(1).expect("the global was pushed");
    value
}
