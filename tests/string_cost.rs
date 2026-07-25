//! Cost-model invariants for data-dependent native library work.

use dellingr::error::{Error, ErrorKind};
use dellingr::{ArgCount, LuaType, RetCount, State};

fn run_cost(source: &str) -> u64 {
    let mut state = State::new();
    state.load_string(source).expect("test program compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("test program runs");
    state.cost_used()
}

fn run_with_budget(source: &str, budget: i64) -> (State, Error) {
    let mut state = State::new();
    state.set_cost_budget(budget);
    state.load_string(source).expect("test program compiles");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("test program exhausts its budget");
    (state, error)
}

#[test]
fn len_is_free_even_for_a_large_string() {
    let subject = "a".repeat(16 * 1024);
    assert_eq!(run_cost(&format!("return string.len('{subject}')")), 0);
}

#[test]
fn captures_have_a_constant_cost_delta_independent_of_subject_size() {
    fn cost(subject_len: usize, pattern: &str) -> u64 {
        let subject = "a".repeat(subject_len);
        run_cost(&format!(
            "for _ = 1, 100 do string.match('{subject}', '{pattern}') end"
        ))
    }

    let short_delta = cost(23, "(%a+)") - cost(23, "%a+");
    let long_delta = cost(1472, "(%a+)") - cost(1472, "%a+");
    assert_eq!(short_delta, long_delta);
    assert_eq!(short_delta, 600);
}

#[cfg(debug_assertions)]
#[test]
fn gmatch_full_scan_is_linear_in_its_subject() {
    fn cost(words: usize) -> (u64, u64) {
        let subject = "a ".repeat(words);
        let mut state = State::new();
        state
            .load_string(format!("for _ in string.gmatch('{subject}', '%a+') do end"))
            .expect("test program compiles");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("test program runs");
        (state.cost_used(), state.gmatch_pattern_compilations())
    }

    let (short, short_compilations) = cost(128);
    let (long, long_compilations) = cost(256);
    assert!(long <= short.saturating_mul(3), "{short} -> {long}");
    assert_eq!(short_compilations, 1);
    assert_eq!(long_compilations, 1);
}

#[test]
fn extracting_a_helper_does_not_change_the_bill() {
    let inlined = run_cost("for _ = 1, 20 do string.upper('abcd') end");
    let extracted = run_cost(
        "local function upper() return string.upper('abcd') end for _ = 1, 20 do upper() end",
    );
    assert_eq!(inlined, extracted);
}

#[test]
fn basic_native_charges_are_exact() {
    for (source, expected) in [
        ("return string.upper('')", 1),
        ("return string.lower('')", 1),
        ("return string.reverse('')", 1),
        ("return string.sub('', 1, 0)", 1),
        ("return string.find('', '')", 2),
        ("return string.match('', '')", 2),
        ("return string.gmatch('', '')", 1),
        ("return string.gsub('', '', '')", 2),
        ("return string.format('')", 1),
        ("return table.concat({}, '')", 2),
        ("return table.concat({}, 'x')", 2),
        ("return table.concat({'a', 'b'}, '')", 7),
        ("return table.concat({'a', 'bb'}, ',')", 10),
    ] {
        assert_eq!(run_cost(source), expected, "{source}");
    }
}

#[test]
fn matcher_refusal_is_a_budget_error_at_the_exact_budget() {
    let mut state = State::new();
    state
        .load_string("iter = string.gmatch('aaaaaaaa', 'a*a*a*a*b')")
        .expect("iterator setup compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("iterator setup runs");
    state.set_cost_budget(17);
    state
        .load_string("return iter()")
        .expect("iterator call compiles");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect_err("pathological matcher exhausts its budget");
    assert!(matches!(error.kind, ErrorKind::BudgetExceeded { .. }));
    assert_eq!(state.cost_used(), 17);
}

#[test]
fn gsub_does_not_return_a_partial_result_and_keeps_callback_effects() {
    let (mut state, error) = run_with_budget(
        "calls = 0; result = string.gsub('aaaa', 'a', function() calls = calls + 1; return 'xx' end)",
        20,
    );
    assert!(matches!(error.kind, ErrorKind::BudgetExceeded { .. }));
    state
        .load_string("return calls, result")
        .expect("query compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(2))
        .expect("query runs after refusal");
    assert!(state.to_number(-2).expect("calls is numeric") > 0.0);
    assert_eq!(state.typ(-1), LuaType::Nil);
}

#[test]
fn gmatch_refusal_does_not_advance_the_iterator() {
    let mut state = State::new();
    state
        .load_string("iter = string.gmatch('one two', '%a+')")
        .expect("iterator setup compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("iterator setup runs");
    state.set_cost_budget(0);
    state
        .load_string("return iter()")
        .expect("iterator call compiles");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect_err("iterator refuses before a match");
    assert!(matches!(error.kind, ErrorKind::BudgetExceeded { .. }));

    state.set_cost_budget(1_000);
    state.load_string("return iter()").expect("retry compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("iterator retry runs");
    assert_eq!(
        state.to_string(-1).expect("iterator returns a string"),
        "one"
    );
}

#[test]
fn gmatch_capture_refusal_does_not_advance_the_iterator() {
    let subject = "a".repeat(4096);
    let setup = format!("iter = string.gmatch('{subject}', '(%a+)')");
    let mut state = State::new();
    state.load_string(&setup).expect("iterator setup compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("iterator setup runs");
    state
        .load_string("return iter()")
        .expect("warm iterator call compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("warm iterator call runs");

    state
        .load_string(&setup)
        .expect("fresh iterator setup compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("fresh iterator setup runs");
    state.set_cost_budget(1_000_000);
    state
        .load_string("return iter()")
        .expect("cost measurement compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("cost measurement runs");
    let successful_cost = state.cost_used();

    state
        .load_string(&setup)
        .expect("retry iterator setup compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("retry iterator setup runs");
    state.set_cost_budget(
        i64::try_from(successful_cost - subject.len() as u64).expect("cost fits in a budget"),
    );
    state
        .load_string("return iter()")
        .expect("refused iterator call compiles");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect_err("capture materialization refuses after matching");
    assert!(matches!(error.kind, ErrorKind::BudgetExceeded { .. }));

    state.set_cost_budget(1_000_000);
    state.load_string("return iter()").expect("retry compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("retry runs");
    assert_eq!(
        state.to_string(-1).expect("iterator returns a string"),
        subject
    );
}

#[test]
fn malformed_gmatch_iterator_compiles_once() {
    let mut state = State::new();
    state
        .load_string("iter = string.gmatch('abc', '%')")
        .expect("iterator setup compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("iterator setup runs");
    for _ in 0..2 {
        state
            .load_string("return iter()")
            .expect("iterator call compiles");
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(1))
            .expect_err("malformed iterator reports its deferred error");
        assert!(matches!(error.kind, ErrorKind::RuntimeError(_)));
    }
    #[cfg(debug_assertions)]
    assert_eq!(state.gmatch_pattern_compilations(), 1);
}

#[test]
fn state_is_reusable_after_a_native_refusal() {
    let (mut state, error) = run_with_budget("return string.upper('abcdef')", 0);
    assert!(matches!(error.kind, ErrorKind::BudgetExceeded { .. }));
    state.set_cost_budget(100);
    state
        .load_string("return string.upper('ok')")
        .expect("retry compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("retry runs");
    assert_eq!(state.to_string(-1).expect("retry result is string"), "OK");
}

#[cfg(feature = "snapshot")]
#[test]
fn refusal_leaves_snapshot_state_quiescent() {
    let mut matcher_state = State::new();
    matcher_state
        .load_string("iter = string.gmatch('aaaa', 'a+')")
        .expect("iterator setup compiles");
    matcher_state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("iterator setup runs");
    matcher_state
        .load_string("return iter()")
        .expect("warm iterator call compiles");
    // Discard the warm result: a retained return value would leave the stack
    // non-empty and fail quiescence for reasons unrelated to the refusal.
    matcher_state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("warm iterator call runs");
    matcher_state
        .load_string("iter = string.gmatch('aaaa', 'a+')")
        .expect("fresh iterator setup compiles");
    matcher_state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("fresh iterator setup runs");
    matcher_state.set_cost_budget(0);
    matcher_state
        .load_string("return iter()")
        .expect("refused iterator call compiles");
    let matcher_error = matcher_state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect_err("matcher refusal occurs after the cache hit");

    let subject = "a".repeat(1024);
    let capture_source = format!("return string.match('{subject}', '(%a+)')");
    let successful_cost = run_cost(&capture_source);
    let (capture_state, capture_error) = run_with_budget(
        &capture_source,
        i64::try_from(successful_cost - subject.len() as u64).expect("cost fits in a budget"),
    );

    let upper = run_with_budget("return string.upper('abcdef')", 0);
    for (label, (state, error)) in [
        ("upper", upper),
        ("matcher", (matcher_state, matcher_error)),
        ("capture", (capture_state, capture_error)),
    ] {
        assert!(matches!(error.kind, ErrorKind::BudgetExceeded { .. }));
        state
            .save_state()
            .unwrap_or_else(|err| panic!("{label} refusal left the state non-quiescent: {err:?}"));
    }
}

#[test]
fn table_concat_cost_is_identical_with_or_without_a_budget() {
    for source in [
        "return table.concat({'a', 'bb'}, ',')",
        "return table.concat({'a', false, 'bb'}, ',')",
    ] {
        let mut count_only = State::new();
        count_only
            .load_string(source)
            .expect("test program compiles");
        // Some cases in this list fail on an invalid element on purpose. The
        // outcome is irrelevant here; only the cost the two modes report is.
        match count_only.call(ArgCount::Fixed(0), RetCount::Fixed(1)) {
            Ok(_) | Err(_) => {}
        }

        let mut finite = State::new();
        finite.set_cost_budget(1_000_000);
        finite.load_string(source).expect("test program compiles");
        match finite.call(ArgCount::Fixed(0), RetCount::Fixed(1)) {
            Ok(_) | Err(_) => {}
        }
        assert_eq!(count_only.cost_used(), finite.cost_used(), "{source}");
    }
}

#[test]
fn count_only_costs_are_deterministic_across_runs() {
    let source = "return string.gsub('ab ab ab', '(%a+)', '%1%1')";
    assert_eq!(run_cost(source), run_cost(source));
}
