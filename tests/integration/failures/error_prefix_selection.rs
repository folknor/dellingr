//! How `Error`'s `Display` picks the location prefix from the selected
//! stack frame.

use dellingr::error::{Error, ErrorKind, PrefixLocation, StackFrame};
use dellingr::{ArgCount, RetCount, State};

fn frame(line: u32) -> StackFrame {
    StackFrame {
        function_name: None,
        source: None,
        line,
        defined_line: 0,
    }
}

/// The level argument a direct host call of `error("boom", ...)` passes.
#[derive(Clone, Copy)]
enum LevelArg {
    Absent,
    Nil,
    One,
}

fn call_error_builtin(level: LevelArg) -> Error {
    let mut state = State::new();
    state.get_global("error").expect("error builtin exists");
    state.push_string("boom").expect("message fits");
    let arguments = match level {
        LevelArg::Absent => 1,
        LevelArg::Nil => {
            state.push_nil().expect("nil fits");
            2
        }
        LevelArg::One => {
            state.push_number(1.0).expect("level fits");
            2
        }
    };
    state
        .call(ArgCount::Fixed(arguments), RetCount::Fixed(0))
        .expect_err("error builtin raises")
}

#[test]
fn direct_host_error_calls_have_no_location_to_distinguish_default_level() {
    let default = call_error_builtin(LevelArg::Absent);
    let explicit_one = call_error_builtin(LevelArg::One);
    let explicit_nil = call_error_builtin(LevelArg::Nil);

    for error in [&default, &explicit_one, &explicit_nil] {
        assert!(error.stack_trace().is_empty());
        assert!(error.source_name().is_none());
        assert_eq!(error.to_string(), "boom");
    }

    assert!(matches!(
        default.kind(),
        ErrorKind::ScriptError {
            prefix: PrefixLocation::Current,
            ..
        }
    ));
    for error in [&explicit_one, &explicit_nil] {
        assert!(matches!(
            error.kind(),
            ErrorKind::ScriptError {
                prefix: PrefixLocation::TraceFrame(0),
                ..
            }
        ));
    }

    let host_error = |prefix| {
        Error::new(
            ErrorKind::ScriptError {
                value: dellingr::error::ErrorValue::String(b"boom".to_vec()),
                prefix,
            },
            4,
            0,
        )
    };
    assert_eq!(host_error(PrefixLocation::Current).to_string(), "4: boom");
    assert_eq!(
        host_error(PrefixLocation::TraceFrame(0)).to_string(),
        "boom"
    );
}

#[test]
fn selected_caller_frame_does_not_borrow_the_innermost_column() {
    let kind = |prefix| ErrorKind::ScriptError {
        value: dellingr::error::ErrorValue::String(b"boom".to_vec()),
        prefix,
    };

    // `error(msg, 2)`: the caller's line must not be paired with the
    // callee's column.
    let caller = Error::new(kind(PrefixLocation::TraceFrame(1)), 1, 7)
        .with_stack_trace(vec![frame(1), frame(2)]);
    assert!(format!("{caller}").starts_with("2: boom"), "{caller}");

    // The innermost frame is the error site, so its column still applies.
    let current =
        Error::new(kind(PrefixLocation::Current), 1, 7).with_stack_trace(vec![frame(1), frame(2)]);
    assert!(format!("{current}").starts_with("1:7: boom"), "{current}");

    let explicit_current =
        Error::new(kind(PrefixLocation::TraceFrame(0)), 1, 7).with_stack_trace(vec![frame(1)]);
    assert!(
        format!("{explicit_current}").starts_with("1:7: boom"),
        "{explicit_current}"
    );

    // Matching line numbers alone do not make a caller the error's own frame.
    let same_line = Error::new(kind(PrefixLocation::TraceFrame(1)), 1, 7)
        .with_stack_trace(vec![frame(1), frame(1)]);
    assert!(format!("{same_line}").starts_with("1: boom"), "{same_line}");

    let different_line =
        Error::new(kind(PrefixLocation::Current), 1, 7).with_stack_trace(vec![frame(2)]);
    assert!(
        format!("{different_line}").starts_with("2: boom"),
        "{different_line}"
    );

    // An unknown selected caller location must not fall back to the callee's
    // site or render a synthetic 0:0 prefix.
    let unknown = Error::new(kind(PrefixLocation::TraceFrame(1)), 1, 7)
        .with_stack_trace(vec![frame(1), frame(0)]);
    assert!(
        format!("{unknown}").starts_with("boom\nstack traceback:"),
        "{unknown}"
    );
}

#[test]
fn unnamed_and_missing_locations_do_not_invent_source_prefixes() {
    let runtime = Error::new(
        ErrorKind::TypeError(dellingr::error::TypeError::Arithmetic(
            dellingr::LuaType::Nil,
        )),
        3,
        0,
    )
    .with_stack_trace(vec![frame(3)]);
    assert!(format!("{runtime}").starts_with("3: "), "{runtime}");

    let stack_frame = frame(3);
    assert_eq!(stack_frame.to_string(), "3: in main chunk");
    assert_eq!(frame(0).to_string(), "in main chunk");

    let unlocated = Error::without_location(ErrorKind::InvalidStackIndex { index: -1 });
    assert_eq!(unlocated.to_string(), "invalid stack index (-1)");
}
