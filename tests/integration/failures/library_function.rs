//! `Error::library_function` names the standard-library function behind every
//! typed library failure that has one, and nothing else.

use std::sync::{Arc, Mutex};

use dellingr::error::{Error, ErrorKind, LibraryError, LibraryFunction, TypeError};
use dellingr::{ArgCount, HostCallbacks, RetCount, State};

use crate::support;

fn assert_library_error(code: &str, function: LibraryFunction) {
    let error = support::run_error(code);
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
    let error = support::run_error(code);
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
fn ipairs_refuses_a_control_whose_successor_is_not_exact() {
    // 2^53 + 1 rounds back to 2^53, so the iterator would revisit one key
    // forever; below -2^53 the step skips keys. Both call paths refuse, the
    // generic-for fast path by deferring to the iterator callback.
    for control in ["2^53", "2^60", "-2^53 - 2"] {
        for code in [
            format!("local f = ipairs({{}}) return f({{[2^53] = 1}}, {control})"),
            format!("local f = ipairs({{}}) for k in f, {{[2^53] = 1}}, {control} do end"),
        ] {
            let error = support::run_error(&code);
            assert!(
                matches!(
                    error.kind(),
                    ErrorKind::LibraryError(LibraryError::IntegerKeyRangeExceeded { .. })
                ),
                "{code}: {:?}",
                error.kind()
            );
            assert_eq!(
                error.library_function(),
                Some(LibraryFunction::Ipairs),
                "{code}"
            );
        }
    }
}

#[test]
fn ipairs_admits_the_controls_at_the_edges_of_the_exact_range() {
    let mut state = State::new();
    support::eval(
        &mut state,
        r#"
        local f = ipairs({})
        local k, v = f({[-2^53 + 1] = "low"}, -2^53)
        if k ~= -2^53 + 1 or v ~= "low" then error("low edge") end
        k, v = f({[2^53] = "high"}, 2^53 - 1)
        if k ~= 2^53 or v ~= "high" then error("high edge") end
        k = f({}, -0.0)
        if k ~= nil then error("negative zero") end
        k, v = f({"one"}, -0.0)
        if k ~= 1 or v ~= "one" then error("negative zero step") end
        return true
        "#,
        1,
    );
    assert!(state.to_boolean(-1));
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
        let error = support::run_error(&code);
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

/// Records the attribution each `on_error` notification carried.
struct AttributionRecorder(Arc<Mutex<Vec<Option<LibraryFunction>>>>);

impl HostCallbacks for AttributionRecorder {
    fn on_print(&mut self, _source: Option<&[u8]>, _line: u32, _message: &[u8]) {}

    fn on_error(&mut self, error: &Error) {
        self.0.lock().unwrap().push(error.library_function());
    }
}

/// Only a comparison `table.sort` itself performs answers `Sort`. A
/// comparison failing in Lua code that sort called (an `__index` read, a
/// comparator) keeps its own attribution, even when that code is itself a
/// default sort; and the one `on_error` notification carries the same
/// attribution as the error the host receives.
#[test]
fn sort_attribution_follows_the_comparison_that_failed() {
    let index_compares = "setmetatable({}, {\n\
         __len = function() return 2 end,\n\
         __index = function() return 1 < 'x' end,\n\
         })";
    let index_sorts = "setmetatable({}, {\n\
         __len = function() return 2 end,\n\
         __index = function() table.sort({1, 'x'}) return 1 end,\n\
         })";
    let cases = [
        (
            "table.sort({1, 'x'})".to_owned(),
            Some(LibraryFunction::Sort),
        ),
        (
            "table.sort(setmetatable({1, 'x'}, {__index = function() end}))".to_owned(),
            Some(LibraryFunction::Sort),
        ),
        (format!("table.sort({index_compares})"), None),
        (
            format!("table.sort({index_compares}, function(a, b) return false end)"),
            None,
        ),
        (
            "table.sort({1, 2}, function(a, b) return a < 'x' end)".to_owned(),
            None,
        ),
        (
            format!("table.sort({index_sorts})"),
            Some(LibraryFunction::Sort),
        ),
        (
            "table.sort({1, 2}, function(a, b) table.sort({1, 'x'}) end)".to_owned(),
            Some(LibraryFunction::Sort),
        ),
    ];
    for (code, expected) in cases {
        let notifications = Arc::new(Mutex::new(Vec::new()));
        let mut state = State::builder()
            .callbacks(Box::new(AttributionRecorder(Arc::clone(&notifications))))
            .build();
        support::load(&mut state, &code);
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err(&format!("expected an error from: {code}"));
        assert!(
            matches!(
                error.kind(),
                ErrorKind::TypeError(TypeError::Comparison(_, _))
            ),
            "{code}: {:?}",
            error.kind()
        );
        assert_eq!(error.library_function(), expected, "{code}");
        assert_eq!(*notifications.lock().unwrap(), vec![expected], "{code}");
    }
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
    let error = support::run_error("next({}, 'missing')");
    assert!(
        matches!(
            error.kind(),
            ErrorKind::LibraryError(LibraryError::InvalidNextKey)
        ),
        "{:?}",
        error.kind()
    );
    assert_eq!(error.library_function(), None);
    let error = support::run_error("table.insert({}, 1.5, 'x')");
    assert!(matches!(error.kind(), ErrorKind::ArgError(_)));
    assert_eq!(error.library_function(), None);
}
