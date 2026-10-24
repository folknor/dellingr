//! How `Error`'s `Display` picks the `line:column:` prefix from the selected
//! stack frame.

use dellingr::error::{Error, ErrorKind, PrefixLocation, StackFrame};

fn frame(line: u32) -> StackFrame {
    StackFrame {
        function_name: None,
        source: None,
        line,
        defined_line: 0,
    }
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
    assert!(format!("{caller}").starts_with("2:0: boom"), "{caller}");

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
    assert!(
        format!("{same_line}").starts_with("1:0: boom"),
        "{same_line}"
    );

    let different_line =
        Error::new(kind(PrefixLocation::Current), 1, 7).with_stack_trace(vec![frame(2)]);
    assert!(
        format!("{different_line}").starts_with("2:0: boom"),
        "{different_line}"
    );

    // An unknown selected caller line must not fall back to the callee's site.
    let unknown = Error::new(kind(PrefixLocation::TraceFrame(1)), 1, 7)
        .with_stack_trace(vec![frame(1), frame(0)]);
    assert!(format!("{unknown}").starts_with("0:0: boom"), "{unknown}");
}
