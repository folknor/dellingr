//! Pins the wording of diagnostics that a script author reads: argument
//! numbers of method calls, traceback labels of unnamed functions, and the
//! argument named by `table.move`'s element-count error.

use dellingr::error::{Error, ErrorKind};
use dellingr::{ArgCount, RetCount, State};

fn run_error(code: &str) -> Error {
    let mut state = State::new();
    dellingr::Program::compile(code, Some(b"t".as_slice()))
        .and_then(|program| state.load(&program))
        .expect("test program compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("test program must fail")
}

fn kind_text(code: &str) -> String {
    run_error(code).kind().to_string()
}

#[test]
fn method_call_arguments_are_numbered_from_the_first_typed_argument() {
    assert_eq!(
        kind_text("local s = 'abc'\ns:sub(1.5)"),
        "bad argument #1 to 'sub' (number has no integer representation)"
    );
    assert_eq!(
        kind_text("local s = 'abc'\ns:sub(1, {})"),
        "bad argument #2 to 'sub' (number expected, got table)"
    );
    // The same function called without `:` keeps the callee's own numbering.
    assert_eq!(
        kind_text("string.sub('abc', 1.5)"),
        "bad argument #2 to 'sub' (number has no integer representation)"
    );
}

#[test]
fn method_call_with_a_bad_receiver_reports_bad_self() {
    assert_eq!(
        kind_text("local t = {sub = string.sub}\nt:sub(1)"),
        "calling 'sub' on bad self (string expected, got table)"
    );
    // A dot call passing the same bad value stays an argument error.
    assert_eq!(
        kind_text("local t = {sub = string.sub}\nt.sub(t, 1)"),
        "bad argument #1 to 'sub' (string expected, got table)"
    );
}

#[test]
fn method_call_numbering_covers_table_functions() {
    assert_eq!(
        kind_text("local t = {concat = table.concat}\nt:concat({})"),
        "bad argument #1 to 'concat' (string expected, got table)"
    );
}

#[test]
fn method_flag_is_exposed_on_the_typed_error() {
    let error = run_error("local s = 'abc'\ns:sub(1.5)");
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected an argument error, got {:?}", error.kind());
    };
    assert!(argument.is_method_call());
    assert_eq!(argument.arg_number(), 2);

    let error = run_error("string.sub('abc', 1.5)");
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected an argument error, got {:?}", error.kind());
    };
    assert!(!argument.is_method_call());
    assert_eq!(argument.arg_number(), 2);
}

#[test]
fn argument_error_inside_a_callback_keeps_its_own_numbering() {
    // The error is raised by a plain call inside the comparator; the outer
    // `t:sort(...)` method call merely propagates it.
    let text = kind_text(
        "local t = {3, 2, 1, sort = table.sort}\n\
         t:sort(function(a, b) return string.sub(a, 1.5) end)",
    );
    assert_eq!(
        text,
        "bad argument #2 to 'sub' (number has no integer representation)"
    );
}

#[test]
fn unnamed_functions_are_labelled_by_where_they_were_defined() {
    let error = run_error("local f = function()\n  error('x')\nend\nf()");
    let frames: Vec<String> = error
        .stack_trace()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        frames,
        ["t:2: in function <t:1>", "t:4: in main chunk"],
        "{error}"
    );
}

#[test]
fn immediately_invoked_function_is_not_a_main_chunk() {
    let error = run_error("(function() error('x') end)()");
    let rendered = error.to_string();
    assert_eq!(rendered.matches("main chunk").count(), 1, "{rendered}");
    assert!(rendered.contains("t:1: in function <t:1>"), "{rendered}");
}

#[test]
fn named_functions_keep_their_names_in_tracebacks() {
    let error = run_error("local function boom()\n  error('x')\nend\nboom()");
    let frames: Vec<String> = error
        .stack_trace()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        frames,
        ["t:2: in function 'boom'", "t:4: in main chunk"],
        "{error}"
    );
}

#[test]
fn table_move_names_the_argument_with_too_many_elements() {
    let error = run_error("table.move({}, -1024, 9223372036854774784, 1)");
    let ErrorKind::ArgError(argument) = error.kind() else {
        panic!("expected an argument error, got {:?}", error.kind());
    };
    assert_eq!(argument.arg_number(), 3);
    assert_eq!(argument.func_name(), Some("move"));
    assert_eq!(
        error.kind().to_string(),
        "bad argument #3 to 'move' (too many elements to move)"
    );
}
