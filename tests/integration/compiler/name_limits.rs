//! Byte limits on source names, snapshot function ids and diagnostic
//! function names: each boundary is accepted, and one byte more is rejected
//! (or, for function names, truncated).

use dellingr::error::ErrorKind;
use dellingr::limits::MAX_SOURCE_NAME_BYTES;
use dellingr::{Program, State};

#[test]
fn source_names_accept_the_limit_and_reject_one_more_byte() {
    let name = vec![b's'; MAX_SOURCE_NAME_BYTES];
    let program =
        Program::compile("return 1", Some(&name)).expect("source name at the limit compiles");
    assert_eq!(program.source_name(), Some(name.as_slice()));
    let mut too_long = name.clone();
    too_long.push(b's');
    let error = Program::compile("return 1", Some(&too_long))
        .expect_err("oversized source name is rejected");
    assert!(matches!(
        error.kind(),
        ErrorKind::SourceNameSizeExceeded { size, limit }
            if *size == MAX_SOURCE_NAME_BYTES + 1 && *limit == MAX_SOURCE_NAME_BYTES
    ));
    // The name is bounded before the source is examined: an oversized name
    // wins over a malformed source.
    let error = Program::compile("local =", Some(&too_long))
        .expect_err("oversized source name is rejected first");
    assert!(matches!(
        error.kind(),
        ErrorKind::SourceNameSizeExceeded { .. }
    ));

    let mut state = State::new();
    state
        .load(&program)
        .expect("load accepts the source-name boundary");
    // A boundary-length source name must also survive saving.
    #[cfg(feature = "snapshot")]
    {
        // Saving requires an empty stack, so drop the loaded chunk.
        state.pop(1).expect("loaded chunk is on the stack");
        state.save_state().expect("boundary source name saves");
    }
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
    assert_eq!(
        error,
        dellingr::SaveError::FieldSizeExceeded {
            field: dellingr::snapshot::SnapshotField::FunctionId,
            size: dellingr::limits::MAX_SAVE_IDENTIFIER_BYTES + 1,
            limit: dellingr::limits::MAX_SAVE_IDENTIFIER_BYTES,
        }
    );
    assert_eq!(
        error.to_string(),
        format!(
            "save function id size {} exceeds limit {}",
            dellingr::limits::MAX_SAVE_IDENTIFIER_BYTES + 1,
            dellingr::limits::MAX_SAVE_IDENTIFIER_BYTES
        )
    );
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
    crate::support::eval(&mut state, source, 1);
    // Saving requires an empty stack, so drop the call result.
    state.pop(1).expect("call result is on the stack");
    state
        .save_state()
        .expect("long diagnostic function name saves");
}
