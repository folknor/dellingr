//! Byte limits on source names, snapshot function ids and diagnostic
//! function names: each boundary is accepted, and one byte more is rejected
//! (or, for function names, truncated) without disturbing the State.

use dellingr::error::ErrorKind;
use dellingr::limits::MAX_SOURCE_NAME_BYTES;
use dellingr::{Engine, State};

#[test]
fn source_names_accept_the_limit_and_reject_one_more_byte() {
    let engine = Engine::new();
    let name = "s".repeat(MAX_SOURCE_NAME_BYTES);
    let program = engine
        .compile_named("return 1", name.clone())
        .expect("source name at the limit compiles");
    assert_eq!(program.source_name(), Some(name.as_str()));
    let too_long = format!("{name}s");
    let error = engine
        .compile_named("return 1", too_long.clone())
        .expect_err("oversized source name is rejected");
    assert!(matches!(
        error.kind(),
        ErrorKind::SourceNameSizeExceeded { size, limit }
            if *size == MAX_SOURCE_NAME_BYTES + 1 && *limit == MAX_SOURCE_NAME_BYTES
    ));

    let mut state = State::new();
    state
        .load_string_named("return 1", Some(name))
        .expect("load accepts the source-name boundary");
    // Saving requires an empty stack, so drop the loaded chunk.
    state.pop(1).expect("loaded chunk is on the stack");
    #[cfg(feature = "snapshot")]
    let before = state
        .save_state()
        .expect("boundary source name saves")
        .bytes;
    let error = state
        .load_string_named("return 2", Some(too_long))
        .expect_err("load rejects oversized source name");
    assert!(matches!(
        error.kind(),
        ErrorKind::SourceNameSizeExceeded { .. }
    ));
    #[cfg(feature = "snapshot")]
    assert_eq!(
        state
            .save_state()
            .expect("rejected load leaves state savable")
            .bytes,
        before
    );
}

#[cfg(feature = "snapshot")]
#[test]
fn registration_limits_are_checked_before_mutation() {
    fn callback(_: &mut State) -> dellingr::Result<u8> {
        Ok(0)
    }

    let mut state = State::new();
    let before = state.save_state().expect("initial state saves").bytes;
    let id = "f".repeat(dellingr::limits::MAX_SAVE_IDENTIFIER_BYTES);
    let error = state
        .register_rust_fn(&format!("{id}f"), callback)
        .expect_err("oversized function id is rejected");
    assert!(matches!(
        error,
        dellingr::SaveError::TooLarge { what: "function id", count }
            if count == dellingr::limits::MAX_SAVE_IDENTIFIER_BYTES + 1
    ));
    assert_eq!(
        state
            .save_state()
            .expect("rejected registration preserves state")
            .bytes,
        before
    );
    state
        .set_global_named_rust_fn("callback", &id, callback)
        .expect("function id at the limit registers");
    state.save_state().expect("boundary function id saves");
}

#[cfg(feature = "snapshot")]
#[test]
fn long_function_identifiers_remain_executable_and_savable() {
    let name = "f".repeat(dellingr::limits::MAX_CHUNK_NAME_BYTES + 1);
    let source = format!("function {name}() return 7 end; return {name}()");
    let mut state = State::new();
    state.load_string(source).expect("long identifier compiles");
    state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(1))
        .expect("truncating diagnostic metadata preserves identifier lookup");
    // Saving requires an empty stack, so drop the call result.
    state.pop(1).expect("call result is on the stack");
    state
        .save_state()
        .expect("long diagnostic function name saves");
}
