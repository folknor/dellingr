//! A host function's diagnostic name is the one its entry was created with.
//! Argument errors raised while it runs name it, whatever the script calls it
//! and wherever it is stored.

use dellingr::error::{Error, ErrorKind};
use dellingr::{ArgCount, LuaType, RetCount, State};

use crate::support;
use crate::support::PushNewRustFn;

fn check_number(state: &mut State) -> dellingr::Result<u8> {
    state.check_type(1, LuaType::Number)?;
    Ok(0)
}

/// Installs `check_number` as global `global`, with diagnostic name `name`.
fn install(state: &mut State, name: &str, global: &str) {
    state
        .push_named_rust_fn(name, check_number)
        .expect("function fits");
    state.set_global(global).expect("global is set");
}

/// The rendered argument error `source` raises.
fn argument_error(state: &mut State, source: &str) -> String {
    let error = support::try_run(state, source).expect_err("the script must fail");
    rendered(&error)
}

fn rendered(error: &Error) -> String {
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected an argument error, got {error}");
    };
    argument.to_string()
}

#[test]
fn a_function_stored_only_in_a_table_renders_its_given_name() {
    let mut state = State::new();
    install(&mut state, "game.spawn", "spawn_impl");
    let message = argument_error(
        &mut state,
        "game = { spawn = spawn_impl }\nspawn_impl = nil\ngame.spawn(false)",
    );
    assert_eq!(
        message,
        "bad argument #1 to 'game.spawn' (number expected, got boolean)"
    );
}

#[test]
fn a_global_alias_does_not_rename_the_function() {
    let mut state = State::new();
    install(&mut state, "real_name", "alias");
    for source in [
        "alias(false)",
        "local other = alias\nother(false)",
        "for _ in alias, false, nil do end",
    ] {
        assert_eq!(
            argument_error(&mut state, source),
            "bad argument #1 to 'real_name' (number expected, got boolean)",
            "{source}"
        );
    }
}

#[test]
fn entries_sharing_a_name_stay_distinct_values() {
    let mut state = State::new();
    let first = state.add_rust_fn("same", check_number).unwrap();
    let second = state.add_rust_fn("same", check_number).unwrap();
    state.push_rust_fn(first).unwrap();
    state.push_rust_fn(second).unwrap();
    state.push_rust_fn(first).unwrap();
    assert!(!state.raw_equal(-3, -2), "two entries are two functions");
    assert!(
        state.raw_equal(-3, -1),
        "one entry pushed twice is one value"
    );
    state.set_global("a").unwrap();
    state.set_global("b").unwrap();
    state.set_global("c").unwrap();
    support::run(
        &mut state,
        "if not (a ~= b and b ~= c and a == c) then error('identity') end",
    );
    // Both still render the shared name.
    for global in ["a", "b", "c"] {
        assert_eq!(
            argument_error(&mut state, &format!("{global}(false)")),
            "bad argument #1 to 'same' (number expected, got boolean)"
        );
    }
}

#[cfg(feature = "snapshot")]
#[test]
fn set_global_named_rust_fn_renders_the_global_name() {
    let mut state = State::new();
    state
        .set_global_named_rust_fn("host_check", "host.check.v1", check_number)
        .expect("the id is new");
    assert_eq!(
        argument_error(&mut state, "host_check(false)"),
        "bad argument #1 to 'host_check' (number expected, got boolean)"
    );
    // Rebinding under another global keeps the name.
    assert_eq!(
        argument_error(&mut state, "other = host_check\nother(false)"),
        "bad argument #1 to 'host_check' (number expected, got boolean)"
    );
}

fn inner_rejects(state: &mut State) -> dellingr::Result<u8> {
    state.check_type(1, LuaType::Number)?;
    Ok(0)
}

fn outer_calls_inner_with_bad_argument(state: &mut State) -> dellingr::Result<u8> {
    state.get_global("inner")?;
    state.push_boolean(false)?;
    state.call(ArgCount::Fixed(1), RetCount::Fixed(0))?;
    Ok(0)
}

fn outer_checks_after_inner_succeeds(state: &mut State) -> dellingr::Result<u8> {
    state.get_global("inner")?;
    state.push_number(1.0)?;
    state.call(ArgCount::Fixed(1), RetCount::Fixed(0))?;
    state.check_type(1, LuaType::Number)?;
    Ok(0)
}

fn check_with_plain_helper(state: &mut State) -> dellingr::Result<u8> {
    plain_helper(state)?;
    Ok(0)
}

fn plain_helper(state: &mut State) -> dellingr::Result<()> {
    state.check_type(1, LuaType::Number)
}

#[test]
fn nested_dispatch_keeps_attribution() {
    let mut state = State::new();
    state
        .push_named_rust_fn("inner_name", inner_rejects)
        .unwrap();
    state.set_global("inner").unwrap();
    state
        .push_named_rust_fn("outer_name", outer_calls_inner_with_bad_argument)
        .unwrap();
    state.set_global("outer_bad").unwrap();
    state
        .push_named_rust_fn("outer_name", outer_checks_after_inner_succeeds)
        .unwrap();
    state.set_global("outer_late").unwrap();

    // An error raised inside the inner callback names the inner function.
    assert_eq!(
        argument_error(&mut state, "outer_bad(1)"),
        "bad argument #1 to 'inner_name' (number expected, got boolean)"
    );
    // Validation in the outer callback after a nested call names the outer one.
    assert_eq!(
        argument_error(&mut state, "outer_late(false)"),
        "bad argument #1 to 'outer_name' (number expected, got boolean)"
    );
}

#[test]
fn a_plain_helper_is_attributed_to_the_callback_that_called_it() {
    let mut state = State::new();
    state
        .push_named_rust_fn("caller_name", check_with_plain_helper)
        .unwrap();
    state.set_global("caller").unwrap();
    assert_eq!(
        argument_error(&mut state, "caller(false)"),
        "bad argument #1 to 'caller_name' (number expected, got boolean)"
    );
}
