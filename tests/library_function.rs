//! `Error::library_function` names the standard-library function behind every
//! typed library failure that has one, and nothing else.

use dellingr::error::{Error, ErrorKind, LibraryError, LibraryFunction, TypeError};
use dellingr::{ArgCount, Program, RetCount, State};

fn run_error(code: &str) -> Error {
    let mut state = State::new();
    Program::compile(code, None)
        .and_then(|program| state.load(&program))
        .expect("test chunk compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err(&format!("expected an error from: {code}"))
}

fn assert_library_error(code: &str, function: LibraryFunction) {
    let error = run_error(code);
    let ErrorKind::LibraryError(library_error) = error.kind() else {
        panic!(
            "expected a LibraryError from {code}, got {:?}",
            error.kind()
        );
    };
    assert_eq!(library_error.function(), Some(function), "{code}");
    assert_eq!(error.library_function(), Some(function), "{code}");
}

fn assert_comparison_error(code: &str, function: Option<LibraryFunction>) {
    let error = run_error(code);
    assert!(
        matches!(
            error.kind(),
            ErrorKind::TypeError(TypeError::Comparison(_, _))
        ),
        "expected a comparison TypeError from {code}, got {:?}",
        error.kind()
    );
    assert_eq!(error.library_function(), function, "{code}");
}

const HUGE_LEN: &str = "setmetatable({}, {__len = function() return 2^60 end})";
const FRACTIONAL_LEN: &str = "setmetatable({}, {__len = function() return 1.5 end})";

#[test]
fn wrong_argument_counts_name_their_function() {
    assert_library_error("table.insert({}, 1, 2, 3)", LibraryFunction::Insert);
    // The metamethod path raises it after evaluating the length.
    assert_library_error(
        "table.insert(setmetatable({}, {__len = function() return 0 end}), 1, 2, 3)",
        LibraryFunction::Insert,
    );
    assert_library_error("math.random(1, 2, 3)", LibraryFunction::Random);
}

#[test]
fn unrepresentable_key_ranges_name_their_function() {
    assert_library_error("table.move({}, 1, 2, 2^53)", LibraryFunction::Move);
    assert_library_error("table.unpack({}, 2^53, 2^53 + 2)", LibraryFunction::Unpack);
    assert_library_error("unpack({}, 2^53, 2^53 + 2)", LibraryFunction::Unpack);
    assert_library_error(
        "table.concat({}, '', 2^53, 2^53 + 2)",
        LibraryFunction::Concat,
    );
    assert_library_error(
        &format!("table.insert({HUGE_LEN}, 1, 'x')"),
        LibraryFunction::Insert,
    );
    assert_library_error(
        &format!("table.remove({HUGE_LEN}, 1)"),
        LibraryFunction::Remove,
    );
}

#[test]
fn non_integer_lengths_name_the_function_that_asked() {
    for (call, function) in [
        ("table.concat(t)", LibraryFunction::Concat),
        ("table.unpack(t)", LibraryFunction::Unpack),
        ("table.insert(t, 'x')", LibraryFunction::Insert),
        ("table.remove(t)", LibraryFunction::Remove),
        ("table.sort(t)", LibraryFunction::Sort),
    ] {
        let code = format!("local t = {FRACTIONAL_LEN}\n{call}");
        let error = run_error(&code);
        assert!(
            matches!(
                error.kind(),
                ErrorKind::LibraryError(LibraryError::ObjectLengthNotInteger { .. })
            ),
            "{code}: {:?}",
            error.kind()
        );
        assert_eq!(error.library_function(), Some(function), "{code}");
    }
}

#[test]
fn concat_values_and_oversized_sorts_name_their_function() {
    assert_library_error("table.concat({{}})", LibraryFunction::Concat);
    let too_big = "setmetatable({}, {__len = function() return 2^40 end})";
    assert_library_error(&format!("table.sort({too_big})"), LibraryFunction::Sort);
    assert_library_error(
        &format!("table.sort({too_big}, function(a, b) return a < b end)"),
        LibraryFunction::Sort,
    );
}

#[test]
fn default_sort_comparison_is_attributed_to_sort() {
    assert_comparison_error("table.sort({1, 'x'})", Some(LibraryFunction::Sort));
    // A metatable with __index sends the sort through its metamethod path.
    assert_comparison_error(
        "table.sort(setmetatable({1, 'x'}, {__index = function() return nil end}))",
        Some(LibraryFunction::Sort),
    );
}

#[test]
fn errors_the_library_only_propagates_are_not_attributed() {
    assert_comparison_error("local x = 1 < 'x'", None);
    assert_comparison_error(
        "table.sort({1, 'x'}, function(a, b) return a < b end)",
        None,
    );
    assert_comparison_error(
        "table.sort(setmetatable({}, {\n\
         __len = function() return 2 end,\n\
         __index = function() return 1 < 'x' end,\n\
         }))",
        None,
    );
    let error = run_error("next({}, 'missing')");
    assert!(
        matches!(
            error.kind(),
            ErrorKind::LibraryError(LibraryError::InvalidNextKey)
        ),
        "{:?}",
        error.kind()
    );
    assert_eq!(error.library_function(), None);
    let error = run_error("table.insert({}, 1.5, 'x')");
    assert!(matches!(error.kind(), ErrorKind::ArgError(_)));
    assert_eq!(error.library_function(), None);
}
