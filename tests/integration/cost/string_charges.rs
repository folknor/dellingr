//! Cost-model invariants for data-dependent native library work.

use dellingr::error::{Error, ErrorKind, LibraryError};
use dellingr::limits::MAX_STRING_BYTES;
use dellingr::{ArgCount, LuaType, RetCount, State};

use super::cost_golden::pinned_cost;
use crate::support;

fn run_with_budget(source: &str, budget: i64) -> (State, Error) {
    let mut state = State::new();
    state.set_cost_budget(budget).expect("the State is metered");
    support::load(&mut state, source);
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("test program exhausts its budget");
    (state, error)
}

#[test]
fn len_is_free_even_for_a_large_string() {
    let subject = "a".repeat(16 * 1024);
    assert_eq!(
        support::cost_of(format!("return string.len('{subject}')")),
        pinned_cost("len_large")
    );
}

/// Capture bytes are produced strings and free; a capture costs only the
/// matcher steps of its open and close items, whatever it captures.
#[test]
fn captures_have_a_constant_cost_delta_independent_of_subject_size() {
    fn cost(subject_len: usize, pattern: &str) -> u64 {
        let subject = "a".repeat(subject_len);
        support::cost_of(format!(
            "for _ = 1, 100 do string.match('{subject}', '{pattern}') end"
        ))
    }

    let short_delta = cost(23, "(%a+)") - cost(23, "%a+");
    let long_delta = cost(1472, "(%a+)") - cost(1472, "%a+");
    assert_eq!(short_delta, long_delta);
    assert_eq!(short_delta, pinned_cost("match_captures"));
}

/// The compile-once half of this check is a crate unit test, since the
/// compilation counter is test-only instrumentation.
#[test]
fn gmatch_full_scan_is_linear_in_its_subject() {
    fn cost(words: usize) -> u64 {
        let subject = "a ".repeat(words);
        support::cost_of(format!("for _ in string.gmatch('{subject}', '%a+') do end"))
    }

    let short = cost(128);
    let long = cost(256);
    assert_eq!(
        long - short,
        pinned_cost("gmatch_subject_doubling"),
        "{short} -> {long}"
    );
}

#[test]
fn extracting_a_helper_does_not_change_the_bill() {
    let inlined = support::cost_of("for _ = 1, 20 do string.upper('abcd') end");
    let extracted = support::cost_of(
        "local function upper() return string.upper('abcd') end for _ = 1, 20 do upper() end",
    );
    assert_eq!(inlined, extracted);
}

#[test]
fn basic_native_charges_are_exact() {
    for (source, expected) in [
        // One unit per call, whatever the bytes read or produced.
        ("return string.upper('')", 1),
        ("return string.upper('abcdef')", 1),
        ("return string.lower('ABC')", 1),
        ("return string.reverse('abc')", 1),
        ("return string.sub('', 1, 0)", 1),
        ("return string.sub('abcdef', 2)", 1),
        ("return string.find('', '')", 1),
        ("return string.match('', '')", 1),
        ("return string.gmatch('', '')", 1),
        ("return string.gsub('', '', '')", 1),
        ("return string.gsub('abcdef', 'x', 'y', 0)", 1),
        // format: 1 per directive, at least 1; literal text is free.
        ("return string.format('')", 1),
        ("return string.format('literal text %% only')", 1),
        ("return string.format('%s', 'abcdef')", 1),
        ("return string.format('%d %5.2f %q', 1, 2, 'x')", 3),
        // Table constructor, its array elements, then concat's flat 1.
        ("return table.concat({}, '')", 2),
        ("return table.concat({}, 'x')", 2),
        ("return table.concat({'a', 'b'}, '')", 4),
        ("return table.concat({'a', 'bb'}, ',')", 4),
    ] {
        assert_eq!(support::cost_of(source), expected, "{source}");
    }
}

/// Output that crosses the string size cap is refused with the size error,
/// and the bytes produced are free either way: both calls charge only their
/// one directive.
#[test]
fn format_output_across_the_size_cap_charges_only_its_directives() {
    fn after_format(format: &str) -> (dellingr::Result<()>, u64) {
        let mut state = State::new();
        state
            .push_bytes(vec![b'a'; MAX_STRING_BYTES - 2])
            .expect("a string under the cap fits");
        state.set_global("s").expect("global set");
        let before = state.cost_used();
        let result = support::try_run(&mut state, format!("string.format('{format}', s)"));
        (result, state.cost_used() - before)
    }

    let (fits, fitting_cost) = after_format("%sxy");
    fits.expect("output exactly at the cap fits");
    let (crosses, crossing_cost) = after_format("%sxyzw");
    let error = crosses.expect_err("output past the cap is refused");
    // The literal run is checked as one append, so the error names the size
    // the whole run would reach.
    assert!(
        matches!(error.kind(), ErrorKind::StringSizeExceeded { size, limit }
            if *size == MAX_STRING_BYTES + 2 && *limit == MAX_STRING_BYTES),
        "{error}"
    );
    assert_eq!(crossing_cost, fitting_cost);
    assert_eq!(fitting_cost, 1);
}

#[test]
fn gsub_rejects_an_invalid_replacement_before_charging() {
    for source in [
        "return string.gsub('subject', 'pattern')",
        "return string.gsub('subject', 'pattern', false)",
    ] {
        let mut state = State::new();
        state.set_cost_budget(0).expect("the State is metered");
        support::load(&mut state, source);
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(1))
            .expect_err("gsub should reject the replacement type");

        assert!(
            error.to_string().contains("bad argument #3 to 'gsub'"),
            "unexpected error: {error}"
        );
        assert_eq!(state.cost_used(), pinned_cost("gsub_invalid_replacement"));
    }
}

/// gmatch reads a leading `^` as a literal byte by compiling it that way, not
/// by rewriting the pattern to `%^`, so `^a` and `%^a` run the same matcher
/// program and, with pattern bytes free, cost the same.
#[test]
fn gmatch_leading_caret_is_charged_as_written() {
    fn cost(pattern: &str) -> u64 {
        support::cost_of(format!(
            "local n = 0 for w in string.gmatch('^a^a', '{pattern}') do n = n + #w end \
             if n ~= 4 then error('two literal-caret matches expected') end"
        ))
    }
    assert_eq!(cost("%^a") - cost("^a"), pinned_cost("gmatch_caret_delta"));
    // 1 for the call, 5 + 5 + 2 matcher steps for two matches and the final
    // miss, and the two loop adds.
    assert_eq!(cost("^a"), 15);
}

/// `find` and `match` have no empty-pattern special case: the plain search
/// and the matcher find an empty pattern for free. Each call here charges
/// only its one call unit, wherever `init` falls.
#[test]
fn empty_find_and_match_charge_only_their_call_unit() {
    for (source, results) in [
        ("return string.find('abc', '', 4)", "4 3"),
        ("return string.find('abc', '', 5)", "nil"),
        ("return string.find('abc', '', 2, true)", "2 1"),
        ("return string.match('abc', '', 3)", ""),
        ("return string.match('abc', '', 5)", "nil"),
    ] {
        let mut state = State::new();
        support::run(
            &mut state,
            format!(
                "local r = table.pack({}) local out = {{}} \
                 for i = 1, r.n do out[i] = tostring(r[i]) end \
                 result = table.concat(out, ' ')",
                source.trim_start_matches("return ")
            ),
        );
        state.get_global("result").expect("result is set");
        assert_eq!(state.to_string(-1).expect("result is a string"), results);
        assert_eq!(
            support::cost_of(source),
            pinned_cost("empty_find_match"),
            "{source}"
        );
    }
}

#[test]
fn matcher_refusal_is_a_budget_error_at_the_exact_budget() {
    let mut state = State::new();
    support::run(&mut state, "iter = string.gmatch('aaaaaaaa', 'a*a*a*a*b')");
    state.set_cost_budget(17).expect("the State is metered");
    support::load(&mut state, "return iter()");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect_err("pathological matcher exhausts its budget");
    assert!(matches!(error.kind(), ErrorKind::BudgetExceeded { .. }));
    assert_eq!(state.cost_used(), pinned_cost("matcher_refusal"));
}

#[test]
fn gsub_does_not_return_a_partial_result_and_keeps_callback_effects() {
    let (mut state, error) = run_with_budget(
        "calls = 0; result = string.gsub('aaaa', 'a', function() calls = calls + 1; return 'xx' end)",
        3,
    );
    assert!(matches!(error.kind(), ErrorKind::BudgetExceeded { .. }));
    support::eval(&mut state, "return calls, result", 2);
    assert!(state.to_number(-2).expect("calls is numeric") > 0.0);
    assert_eq!(state.typ(-1), LuaType::Nil);
}

#[test]
fn gmatch_refusal_does_not_advance_the_iterator() {
    let mut state = State::new();
    support::run(&mut state, "iter = string.gmatch('one two', '%a+')");
    state.set_cost_budget(0).expect("the State is metered");
    support::load(&mut state, "return iter()");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect_err("iterator refuses before a match");
    assert!(matches!(error.kind(), ErrorKind::BudgetExceeded { .. }));

    state.set_cost_budget(1_000).expect("the State is metered");
    support::eval(&mut state, "return iter()", 1);
    assert_eq!(
        state.to_string(-1).expect("iterator returns a string"),
        "one"
    );
}

/// The compile-once half of this check is a crate unit test, since the
/// compilation counter is test-only instrumentation.
#[test]
fn malformed_gmatch_iterator_repeats_its_deferred_error() {
    let mut state = State::new();
    support::run(&mut state, "iter = string.gmatch('abc', '%')");
    for _ in 0..2 {
        support::load(&mut state, "return iter()");
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(1))
            .expect_err("malformed iterator reports its deferred error");
        assert!(matches!(error.kind(), ErrorKind::PatternError(_)));
    }
}

#[test]
fn gsub_keeps_its_pattern_across_collections_in_its_replacement() {
    let mut state = State::new();
    support::eval(
        &mut state,
        "local junk
             local out, n = string.gsub('a1b2c3d4e5f6', '(%a)(%d)',
                 function(a, d)
                     for i = 1, 200 do junk = {i, tostring(i)} end
                     local inner = string.gsub(a .. d, '(%a)(%d)', '%2%1')
                     return inner
                 end)
             return out .. ':' .. n",
        1,
    );
    assert_eq!(
        state.to_string(-1).expect("gsub result is a string"),
        "1a2b3c4d5e6f:6"
    );
}

#[test]
fn state_is_reusable_after_a_native_refusal() {
    let (mut state, error) = run_with_budget("return string.upper('abcdef')", 0);
    assert!(matches!(error.kind(), ErrorKind::BudgetExceeded { .. }));
    state.set_cost_budget(100).expect("the State is metered");
    support::eval(&mut state, "return string.upper('ok')", 1);
    assert_eq!(state.to_string(-1).expect("retry result is string"), "OK");
}

#[cfg(feature = "snapshot")]
#[test]
fn refusal_leaves_snapshot_state_quiescent() {
    let mut matcher_state = State::new();
    support::run(&mut matcher_state, "iter = string.gmatch('aaaa', 'a+')");
    support::load(&mut matcher_state, "return iter()");
    // Discard the warm result: a retained return value would leave the stack
    // non-empty and fail quiescence for reasons unrelated to the refusal.
    matcher_state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("warm iterator call runs");
    support::run(&mut matcher_state, "iter = string.gmatch('aaaa', 'a+')");
    matcher_state
        .set_cost_budget(0)
        .expect("the State is metered");
    support::load(&mut matcher_state, "return iter()");
    let matcher_error = matcher_state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect_err("matcher refusal occurs after the cache hit");

    let upper = run_with_budget("return string.upper('abcdef')", 0);
    for (label, (state, error)) in [
        ("upper", upper),
        ("matcher", (matcher_state, matcher_error)),
    ] {
        assert!(matches!(error.kind(), ErrorKind::BudgetExceeded { .. }));
        state
            .save_state()
            .unwrap_or_else(|err| panic!("{label} refusal left the state non-quiescent: {err:?}"));
    }
}

#[test]
fn table_concat_cost_is_identical_with_or_without_a_budget() {
    for (source, expected_cost, expect_invalid_value) in [
        ("return table.concat({'a', 'bb'}, ',')", 4, false),
        ("return table.concat({'a', false, 'bb'}, ',')", 5, true),
    ] {
        for budget in [None, Some(1_000_000)] {
            let mut state = State::new();
            if let Some(budget) = budget {
                state.set_cost_budget(budget).expect("the State is metered");
            }
            support::load(&mut state, source);
            let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(1));
            if expect_invalid_value {
                let error = result.expect_err("concat rejects a boolean element");
                assert!(matches!(
                    error.kind(),
                    ErrorKind::LibraryError(LibraryError::InvalidConcatValue {
                        value_type: LuaType::Boolean,
                        index: 2,
                    })
                ));
            } else {
                result.expect("concat accepts string elements");
                assert_eq!(
                    state.to_string(-1).expect("concat returns a string"),
                    "a,bb"
                );
            }
            assert_eq!(state.cost_used(), expected_cost, "{source}");
        }
    }
}

#[test]
fn gsub_count_only_cost_matches_between_fresh_states() {
    let source = "return string.gsub('ab ab ab', '(%a+)', '%1%1')";
    assert_eq!(support::cost_of(source), support::cost_of(source));
}
