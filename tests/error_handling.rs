//! Tests for proper error handling (things that used to panic).
//!
//! These tests verify that the VM returns clean errors instead of panicking
//! when it encounters unexpected types or corrupt state.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, LuaType, RetCount, State};

/// Helper: runs Lua code that returns a number, checks the result.
fn run_number(code: &str) -> f64 {
    let mut state = State::new();
    state.load_string(code).unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .unwrap_or_else(|e| panic!("Error running: {code}\n{e}"));
    state.to_number(-1).unwrap()
}

/// Helper: runs a Lua string and returns the error, panicking if it succeeds.
fn expect_error(code: &str) -> dellingr::error::Error {
    let mut state = State::new();
    state.load_string(code).unwrap();
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    result.expect_err(&format!("Expected error from: {code}"))
}

fn assert_invalid_next_key(code: &str) {
    let err = expect_error(code);
    match err.kind {
        ErrorKind::RuntimeError(message) => assert_eq!(message, "invalid key to 'next'"),
        kind => panic!("Expected invalid next key runtime error, got: {kind:?}"),
    }
}

fn assert_pattern_runtime_error(code: &str, message: &str) {
    let err = expect_error(code);
    match err.kind {
        ErrorKind::RuntimeError(actual) => assert_eq!(actual, message),
        kind => panic!("Expected pattern runtime error, got: {kind:?}"),
    }
}

#[test]
fn malformed_pattern_capture_errors_are_runtime_errors() {
    assert_pattern_runtime_error(r#"string.match("aa", "(a)%0")"#, "invalid capture index %0");
    assert_pattern_runtime_error(r#"string.match("a", ")")"#, "invalid pattern capture");
}

#[test]
fn call_rejects_missing_fixed_arguments_without_panicking() {
    let mut state = State::new();
    state.push_rust_fn(|_state| Ok(0));

    let err = state
        .call(ArgCount::Fixed(1), RetCount::Fixed(0))
        .expect_err("missing fixed argument must return an error");
    assert!(matches!(
        err.kind,
        ErrorKind::InvalidStackIndex { index: -2 }
    ));
    assert_eq!(state.get_top(), 1);
    assert_eq!(state.typ(-1), LuaType::Function);
}

#[test]
fn public_call_rejects_dynamic_without_base_without_panicking() {
    let mut state = State::new();
    state.push_rust_fn(|_state| Ok(0));

    let err = state
        .call(ArgCount::Dynamic, RetCount::Fixed(0))
        .expect_err("host Dynamic call without a base must return an error");
    assert!(matches!(err.kind, ErrorKind::InternalError(_)));
    assert_eq!(state.get_top(), 1);
    assert_eq!(state.typ(-1), LuaType::Function);
}

// -- Numeric for-loop tests --

#[test]
fn numeric_for_loop_basic() {
    let sum = run_number(
        r#"
        local sum = 0
        for i = 1, 10 do
            sum = sum + i
        end
        return sum
    "#,
    );
    assert_eq!(sum, 55.0);
}

#[test]
fn numeric_for_loop_step() {
    let sum = run_number(
        r#"
        local sum = 0
        for i = 0, 10, 2 do
            sum = sum + i
        end
        return sum
    "#,
    );
    assert_eq!(sum, 30.0);
}

#[test]
fn numeric_for_loop_negative_step() {
    let sum = run_number(
        r#"
        local sum = 0
        for i = 10, 1, -1 do
            sum = sum + i
        end
        return sum
    "#,
    );
    assert_eq!(sum, 55.0);
}

#[test]
fn numeric_for_loop_empty_range() {
    let count = run_number(
        r#"
        local count = 0
        for i = 10, 1 do
            count = count + 1
        end
        return count
    "#,
    );
    assert_eq!(count, 0.0);
}

// -- Generic for-loop (ipairs) tests --

#[test]
fn ipairs_basic() {
    let sum = run_number(
        r#"
        local t = {10, 20, 30}
        local sum = 0
        for i, v in ipairs(t) do
            sum = sum + v
        end
        return sum
    "#,
    );
    assert_eq!(sum, 60.0);
}

#[test]
fn ipairs_stops_at_nil() {
    let count = run_number(
        r#"
        local t = {10, 20, nil, 40}
        local count = 0
        for i, v in ipairs(t) do
            count = count + 1
        end
        return count
    "#,
    );
    assert_eq!(count, 2.0);
}

#[test]
fn ipairs_uses_index_metamethod_for_holes() {
    let sum = run_number(
        r#"
        local t = setmetatable({10}, {
            __index = function(self, key)
                if key == 2 then return 20 end
                return nil
            end
        })
        local sum = 0
        for i, v in ipairs(t) do
            sum = sum + v
        end
        return sum
    "#,
    );
    assert_eq!(sum, 30.0);
}

// -- Error type tests --

#[test]
fn error_function_produces_error() {
    let err = expect_error("error('user error message')");
    let msg = format!("{err}");
    assert!(
        msg.contains("user error message"),
        "Error should contain user message, got: {msg}"
    );
}

#[test]
fn type_error_on_arithmetic() {
    let err = expect_error("local x = 'hello' + 1");
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError, got: {err}"
    );
}

#[test]
fn type_error_on_call() {
    let err = expect_error("local x = 5\nx()");
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError, got: {err}"
    );
}

#[test]
fn type_error_on_index() {
    let err = expect_error("local x = 5\nlocal y = x.foo");
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError, got: {err}"
    );
}

#[test]
fn type_error_on_table_key_nil() {
    let err = expect_error("local t = {}\nt[nil] = 1");
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError, got: {err}"
    );
}

#[test]
fn budget_exceeded_error() {
    let mut state = State::new();
    state.set_cost_budget(10);
    state
        .load_string(
            r#"
        local sum = 0
        for i = 1, 10000 do
            sum = sum + i
        end
    "#,
        )
        .unwrap();
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    let err = result.expect_err("Expected budget error");
    assert!(
        matches!(err.kind, ErrorKind::BudgetExceeded { .. }),
        "Expected BudgetExceeded, got: {err}"
    );
}

#[test]
fn budget_stops_at_the_first_operation_after_exhaustion() {
    for budget in [1, 63, 65] {
        let mut state = State::new();
        state.set_cost_budget(budget);
        let increments = budget + 2;
        state
            .load_string(format!(
                "x = 0\n{}",
                "x = x + 1\n".repeat(increments as usize)
            ))
            .expect("test program should load");

        let err = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("the operation after the exhausted budget must fail");
        assert!(matches!(err.kind, ErrorKind::BudgetExceeded { .. }));

        state.get_global("x");
        assert_eq!(
            state.to_number(-1).expect("x should be numeric"),
            budget as f64
        );
        assert_eq!(state.cost_used(), budget as u64);
        assert_eq!(state.cost_remaining(), budget - budget);
    }
}

#[test]
fn budget_flushes_pending_caller_cost_before_nested_call() {
    let mut state = State::new();
    state.set_cost_budget(1);
    state
        .load_string("x = 0\nlocal function f() x = x + 1 end\nx = x + 1\nf()\nx = x + 1")
        .expect("test program should load");

    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("callee must observe the caller's pending cost");
    assert!(matches!(err.kind, ErrorKind::BudgetExceeded { .. }));
    state.get_global("x");
    assert_eq!(state.to_number(-1).expect("x should be numeric"), 1.0);
    assert_eq!(state.cost_used(), 1);
    assert_eq!(state.cost_remaining(), 0);
}

#[test]
fn call_depth_exceeded_error() {
    let err = expect_error(
        r#"
        local function recurse(n)
            return recurse(n + 1)
        end
        recurse(0)
    "#,
    );
    assert!(
        matches!(err.kind, ErrorKind::CallDepthExceeded { .. }),
        "Expected CallDepthExceeded, got: {err}"
    );
}

// -- Table operation tests --

#[test]
fn table_insert_append() {
    let val = run_number(
        r#"
        local t = {1, 2, 3}
        table.insert(t, 4)
        return #t
    "#,
    );
    assert_eq!(val, 4.0);
}

#[test]
fn table_insert_at_position() {
    let val = run_number(
        r#"
        local t = {1, 2, 3}
        table.insert(t, 2, 99)
        return t[2]
    "#,
    );
    assert_eq!(val, 99.0);
}

#[test]
fn tonumber_uses_lua_numeral_grammar() {
    let value = run_number(
        r#"
        local a = tonumber(" \t+42\n")
        local b = tonumber(".5e1")
        local c = tonumber("0x10")
        local d = tonumber("-0X1.8p+1")
        local e = tonumber("10", nil)
        if tonumber("nan") ~= nil or tonumber("NaN") ~= nil or tonumber("inf") ~= nil
            or tonumber("1_0") ~= nil or tonumber("0b10") ~= nil or tonumber("1e") ~= nil then
            return -1
        end
        return a + b + c + d + e
    "#,
    );
    assert_eq!(value, 70.0);
}

#[test]
fn pairs_uses_builtin_next_after_rebinding() {
    let value = run_number(
        r#"
        next = 42
        local sum = 0
        for _, value in pairs({ a = 2, b = 3 }) do sum = sum + value end
        for _, value in ipairs({ 4, 5 }) do sum = sum + value end
        return sum
    "#,
    );
    assert_eq!(value, 14.0);
}

#[test]
fn next_rejects_invalid_controls() {
    for table in ["{1, 2, 3}", "{1, 2, 3, 4, 5}"] {
        assert_invalid_next_key(&format!("local t = {table}; next(t, 99)"));
        assert_invalid_next_key(&format!("local t = {table}; next(t, 0 / 0)"));
    }
}

#[test]
fn generic_for_next_rejects_invalid_controls() {
    for table in ["{1, 2, 3}", "{1, 2, 3, 4, 5}"] {
        assert_invalid_next_key(&format!("local t = {table}; for _ in next, t, 99 do end"));
        assert_invalid_next_key(&format!(
            "local t = {table}; for _ in next, t, 0 / 0 do end"
        ));
    }
}

#[test]
fn table_position_boundaries_and_integer_errors() {
    let value = run_number(
        r#"
        local t = {10, 20}
        table.insert(t, 3, 30)
        local no_op = table.remove(t, 4) == nil
        local empty = {}
        local e0 = table.remove(empty, 0) == nil
        local e1 = table.remove(empty, 1) == nil
        return #t * 100 + t[3] + (no_op and 1 or 0) + (e0 and 2 or 0) + (e1 and 4 or 0)
    "#,
    );
    assert_eq!(value, 337.0);
    for code in [
        "table.insert({}, 0, 1)",
        "table.insert({}, 1.5, 1)",
        "table.insert({}, 0 / 0, 1)",
        "table.remove({1}, 1e100)",
        "table.remove({}, 2)",
    ] {
        let err = expect_error(code);
        assert!(
            matches!(err.kind, ErrorKind::RuntimeError(_)),
            "{code}: {err}"
        );
    }
    // A negative position is a valid integer that is simply out of range: it
    // must report "position out of bounds", not "no integer representation".
    for code in ["table.insert({1}, -1, 9)", "table.remove({1}, -1)"] {
        let err = expect_error(code);
        assert!(
            err.to_string().contains("position out of bounds"),
            "{code}: {err}"
        );
    }
}

#[test]
fn table_insert_move_and_random_validate_before_mutation() {
    for code in ["table.insert({})", "table.insert({}, 1, 2, 3)"] {
        let err = expect_error(code);
        assert!(
            matches!(err.kind, ErrorKind::RuntimeError(_)),
            "{code}: {err}"
        );
    }
    let err = expect_error("local t = {7}; table.move(t, 1, 1, 2, 42)");
    assert!(matches!(err.kind, ErrorKind::ArgError(_)), "{err}");
    for code in [
        "math.random(0)",
        "math.random(2, 1)",
        "math.random(1, 2, 3)",
    ] {
        let err = expect_error(code);
        assert!(
            matches!(err.kind, ErrorKind::RuntimeError(_)),
            "{code}: {err}"
        );
    }
    let mut first = State::new();
    let mut second = State::new();
    first.set_rng_seed(99);
    second.set_rng_seed(99);
    for state in [&mut first, &mut second] {
        state
            .load_string("return math.random() + math.random(1, 10)")
            .unwrap();
        state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    }
    assert_eq!(first.to_number(-1).unwrap(), second.to_number(-1).unwrap());
}

#[test]
fn table_remove_basic() {
    let val = run_number(
        r#"
        local t = {10, 20, 30}
        local removed = table.remove(t, 2)
        return removed
    "#,
    );
    assert_eq!(val, 20.0);
}

#[test]
fn table_nil_assignment_deletes_key() {
    let val = run_number(
        r#"
        local t = {a = 1, b = 2}
        t.a = nil

        if rawget(t, "a") ~= nil then
            return -1
        end

        local count = 0
        local saw_a = 0
        for k, v in pairs(t) do
            count = count + 1
            if k == "a" then
                saw_a = saw_a + 1
            end
        end

        return count * 10 + saw_a
    "#,
    );
    assert_eq!(val, 10.0);
}

#[test]
fn table_sort_basic() {
    let val = run_number(
        r#"
        local t = {3, 1, 2}
        table.sort(t)
        return t[1] * 100 + t[2] * 10 + t[3]
    "#,
    );
    assert_eq!(val, 123.0);
}

#[test]
fn table_sort_with_comparator() {
    let val = run_number(
        r#"
        local t = {1, 2, 3}
        table.sort(t, function(a, b) return a > b end)
        return t[1] * 100 + t[2] * 10 + t[3]
    "#,
    );
    assert_eq!(val, 321.0);
}

#[test]
fn table_sort_charges_before_mutating() {
    // At an exhausted budget the sort must be blocked BEFORE it mutates the
    // table or runs the comparator (L18). table_sort charges its cost up front,
    // so with budget 0 it errors instead of sorting.
    let mut state = State::new();
    state.load_string("return {3, 1, 2}").unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();

    state.set_cost_budget(0);
    let err = state
        .table_sort(1, false)
        .expect_err("exhausted budget must block the sort");
    assert!(matches!(err.kind, ErrorKind::BudgetExceeded { .. }));

    // Restore budget and confirm the table is untouched: still {3, 1, 2}.
    state.set_cost_budget(i64::MAX);
    state.push_number(1.0);
    state.get_table(1).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 3.0);
}

#[test]
fn table_concat_basic() {
    let mut state = State::new();
    state
        .load_string(r#"return table.concat({1, 2, 3}, ", ")"#)
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_string(-1).unwrap(), "1, 2, 3");
}

#[test]
fn table_concat_rejects_boolean_elements() {
    let err = expect_error(r#"return table.concat({true}, ",")"#);
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError, got: {err}"
    );
}

#[test]
fn table_concat_rejects_nil_elements_in_range() {
    let err = expect_error(r#"return table.concat({1, nil, 3}, ",", 1, 3)"#);
    assert!(
        matches!(err.kind, ErrorKind::TypeError(_)),
        "Expected TypeError, got: {err}"
    );
}

#[test]
fn table_unpack_basic() {
    let val = run_number(
        r#"
        local a, b, c = table.unpack({10, 20, 30})
        return a + b + c
    "#,
    );
    assert_eq!(val, 60.0);
}

#[test]
fn global_unpack_supports_range() {
    let val = run_number(
        r#"
        local a, b, c = unpack({10, 20, 30, 40}, 2, 4)
        return a + b + c
    "#,
    );
    assert_eq!(val, 90.0);
}

#[test]
fn table_move_overlapping_same_table_copies_backwards() {
    let val = run_number(
        r#"
        local t = {1, 2, 3, 4, 5}
        table.move(t, 1, 3, 2)
        return t[1] * 10000 + t[2] * 1000 + t[3] * 100 + t[4] * 10 + t[5]
    "#,
    );
    assert_eq!(val, 11235.0);
}

#[test]
fn table_move_explicit_same_destination_copies_backwards() {
    let val = run_number(
        r#"
        local t = {1, 2, 3, 4, 5}
        table.move(t, 1, 3, 2, t)
        return t[1] * 10000 + t[2] * 1000 + t[3] * 100 + t[4] * 10 + t[5]
    "#,
    );
    assert_eq!(val, 11235.0);
}

#[test]
fn table_move_overlapping_same_table_copies_forwards() {
    let val = run_number(
        r#"
        local t = {1, 2, 3, 4, 5}
        table.move(t, 2, 4, 1)
        return t[1] * 10000 + t[2] * 1000 + t[3] * 100 + t[4] * 10 + t[5]
    "#,
    );
    assert_eq!(val, 23445.0);
}

#[test]
fn table_move_costs_empty_and_each_moved_element() {
    for (code, expected_cost) in [
        ("return table.move({}, 1, 0, 1)", 2),
        ("return table.move({}, 1, 1, 1)", 2),
        ("return table.move({}, 1, 3, 1)", 4),
    ] {
        let mut state = State::new();
        state
            .load_string(code)
            .expect("table.move program compiles");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(1))
            .expect("table.move program runs");
        assert_eq!(state.cost_used(), expected_cost, "{code}");
    }

    let mut state = State::new();
    state.set_cost_budget(0);
    // Call the native function directly so bytecode dispatch cannot consume
    // the exhausted budget before table.move reaches its empty-range charge.
    state.get_global("table");
    state.push_bytes("move");
    state.get_table(-2).expect("table.move lookup succeeds");
    state.remove(-2).expect("table table is removed");
    state.new_table();
    state.push_number(2.0);
    state.push_number(1.0);
    state.push_number(1.0);
    let error = state
        .call(ArgCount::Fixed(4), RetCount::Fixed(1))
        .expect_err("empty table.move must charge a configured exhausted budget");
    assert!(matches!(error.kind, ErrorKind::BudgetExceeded { .. }));
}

#[test]
fn table_move_rejects_overflow_ranges_before_mutating() {
    let mut state = State::new();
    state
        .load_string("t = {10, 20, 30}")
        .expect("setup compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("setup runs");

    for (code, message) in [
        (
            "table.move(t, -1024, 9223372036854774784, 1)",
            "too many elements to move",
        ),
        (
            "table.move(t, 1, 2000, 9223372036854774784)",
            "destination wrap around",
        ),
    ] {
        state.load_string(code).expect("overflow program compiles");
        let err = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("overflow range must fail cleanly");
        assert!(matches!(err.kind, ErrorKind::RuntimeError(ref got) if got == message));

        state.get_global("t");
        for (index, expected) in [10.0, 20.0, 30.0].into_iter().enumerate() {
            state.push_number((index + 1) as f64);
            state.get_table(-2).expect("table read succeeds");
            assert_eq!(
                state.to_number(-1).expect("table value is numeric"),
                expected
            );
            state.pop(1);
        }
        state.pop(1);
    }
}

#[test]
fn table_move_rejects_out_of_range_numbers_cleanly() {
    let error = expect_error("table.move({}, -1e300, 1e300, 1)");
    assert!(matches!(error.kind, ErrorKind::RuntimeError(ref message)
        if message == "bad argument #2 to 'move' (number has no integer representation)"));
}

fn table_after_budgeted_move(budget: i64) -> (dellingr::error::Error, Vec<f64>) {
    let mut state = State::new();
    state
        .load_string("t = {1, 2, 3, 4, 5}")
        .expect("setup compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("setup runs");
    state.set_cost_budget(budget);
    state
        .load_string("table.move(t, 1, 4, 2)")
        .expect("move program compiles");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("limited move must stop at the exhausted budget");

    state.get_global("t");
    let mut values = Vec::new();
    for index in 1..=5 {
        state.push_number(index as f64);
        state.get_table(-2).expect("table read succeeds");
        values.push(state.to_number(-1).expect("table value is numeric"));
        state.pop(1);
    }
    (error, values)
}

#[test]
fn table_move_budget_zero_does_not_mutate() {
    let (error, values) = table_after_budgeted_move(0);
    assert!(matches!(error.kind, ErrorKind::BudgetExceeded { .. }));
    assert_eq!(values, [1.0, 2.0, 3.0, 4.0, 5.0]);
}

#[test]
fn table_move_budget_partial_mutation_is_deterministic() {
    let first = table_after_budgeted_move(2);
    let second = table_after_budgeted_move(2);
    assert!(matches!(first.0.kind, ErrorKind::BudgetExceeded { .. }));
    assert!(matches!(second.0.kind, ErrorKind::BudgetExceeded { .. }));
    assert_eq!(first.1, [1.0, 2.0, 3.0, 3.0, 4.0]);
    assert_eq!(first.1, second.1);
}

#[test]
fn table_move_cost_is_deterministic_across_fresh_states() {
    let run = || {
        let mut state = State::new();
        state
            .load_string("local t = {1, 2, 3}; table.move(t, 1, 3, 4)")
            .expect("program compiles");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("program runs");
        state.cost_used()
    };
    assert_eq!(run(), run());
}

#[test]
fn select_negative_index_counts_from_end() {
    let val = run_number(
        r#"
        local a, b, c = select(-2, "a", "b", "c")
        if a == "b" and b == "c" and c == nil then
            return 1
        end
        return 0
    "#,
    );
    assert_eq!(val, 1.0);
}

#[test]
fn select_zero_index_errors() {
    let err = expect_error(r#"select(0, "a", "b")"#);
    assert!(
        matches!(err.kind, ErrorKind::ArgError(_)),
        "Expected ArgError, got: {err}"
    );
}

#[test]
fn select_too_negative_index_errors() {
    let err = expect_error(r#"select(-4, "a", "b", "c")"#);
    assert!(
        matches!(err.kind, ErrorKind::ArgError(_)),
        "Expected ArgError, got: {err}"
    );
}

#[test]
fn tonumber_parses_base_argument() {
    let val = run_number(
        r#"
        return tonumber("ff", 16) + tonumber("-10", 16) + tonumber("z", 36)
    "#,
    );
    assert_eq!(val, 274.0);
}

#[test]
fn tonumber_base_invalid_digit_returns_nil() {
    let val = run_number(
        r#"
        if tonumber("102", 2) == nil then
            return 1
        end
        return 0
    "#,
    );
    assert_eq!(val, 1.0);
}

#[test]
fn tonumber_base_out_of_range_errors() {
    let err = expect_error(r#"return tonumber("10", 37)"#);
    assert!(
        matches!(err.kind, ErrorKind::ArgError(_)),
        "Expected ArgError, got: {err}"
    );
}

#[test]
fn tonumber_base_requires_string_input() {
    let err = expect_error(r#"return tonumber(10, 10)"#);
    assert!(
        matches!(err.kind, ErrorKind::ArgError(_)),
        "Expected ArgError, got: {err}"
    );
}

// -- Error message quality tests --

/// Helper: tries to load+call Lua code and returns the error, panicking if it succeeds.
/// Uses RetCount::Void since we only care about the error.
fn expect_load_or_run_error(code: &str) -> dellingr::error::Error {
    let mut state = State::new();
    if let Err(e) = state.load_string(code) {
        return e;
    }
    let result = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
    result.expect_err(&format!("Expected error from: {code}"))
}

#[test]
fn error_msg_unexpected_token_includes_context() {
    // Using 'end' where an expression is expected
    let err = expect_load_or_run_error("local x = end");
    let msg = format!("{err}");
    assert!(
        msg.contains("expected") || msg.contains("near"),
        "Error should describe what was expected, got: {msg}"
    );
}

#[test]
fn error_msg_vararg_outside_vararg_function() {
    // '...' inside a non-vararg function should error
    let err = expect_load_or_run_error("local function f() return ... end");
    let msg = format!("{err}");
    assert!(
        msg.contains("vararg"),
        "Error should mention vararg, got: {msg}"
    );
}

#[test]
fn error_msg_vararg_in_table_outside_vararg_function() {
    let err = expect_load_or_run_error("local function f() return {...} end");
    let msg = format!("{err}");
    assert!(
        msg.contains("vararg"),
        "Error should mention vararg, got: {msg}"
    );
}

#[test]
fn error_msg_missing_end_keyword() {
    let err = expect_load_or_run_error("if true then local x = 1");
    let msg = format!("{err}");
    // Should get unexpected EOF (missing 'end')
    assert!(
        msg.contains("<eof>") || msg.contains("expected"),
        "Error should mention <eof> or expected, got: {msg}"
    );
}

#[test]
fn error_msg_type_error_arithmetic() {
    let err = expect_error("local x = 'hello' + 1");
    let msg = format!("{err}");
    assert!(
        msg.contains("arithmetic") && msg.contains("string"),
        "Arithmetic type error should mention 'arithmetic' and 'string', got: {msg}"
    );
}

#[test]
fn error_msg_type_error_call() {
    let err = expect_error("local x = 5\nx()");
    let msg = format!("{err}");
    assert!(
        msg.contains("call") && msg.contains("number"),
        "Call type error should mention 'call' and 'number', got: {msg}"
    );
}

#[test]
fn error_msg_type_error_index() {
    let err = expect_error("local x = 5\nlocal y = x.foo");
    let msg = format!("{err}");
    assert!(
        msg.contains("index") && msg.contains("number"),
        "Index type error should mention 'index' and 'number', got: {msg}"
    );
}

#[test]
fn error_msg_budget_exceeded_shows_amounts() {
    let mut state = State::new();
    state.set_cost_budget(10);
    state
        .load_string("local s = 0\nfor i = 1, 10000 do s = s + i end")
        .unwrap();
    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("Expected budget error");
    let msg = format!("{err}");
    assert!(
        msg.contains("budget") && msg.contains("10"),
        "Budget error should show budget amount, got: {msg}"
    );
}

#[test]
fn global_lookup_cache_respects_restricted_env() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            x = 1
            function read_x()
                if x == nil then return 2 end
                return x
            end
        "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    state.get_global("read_x");
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 1.0);
    state.pop(1);

    let restricted = state.with_restricted_env(&["read_x"], |state| {
        state.get_global("read_x");
        state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
        let result = state.to_number(-1).unwrap();
        state.pop(1);
        result
    });
    assert_eq!(restricted, 2.0);

    state.get_global("read_x");
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 1.0);
}

#[test]
fn restricted_env_restored_after_panic() {
    // A panic inside the closure must still restore the full environment (L11),
    // so a caller that catches the panic can reuse the State. `math` is not in
    // the whitelist, so it is nil during the closure but must be back after.
    let mut state = State::new();
    state.new_table();
    state.set_global("saved_object");

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {})); // silence the expected panic
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state.with_restricted_env(&["print"], |state| {
            state.gc_collect();
            panic!("boom inside restricted env");
        })
    }));
    std::panic::set_hook(prev_hook);
    assert!(caught.is_err(), "the panic must propagate");

    // Environment restored: a non-whitelisted global is available again.
    state.get_global("math");
    assert_eq!(state.typ(-1), LuaType::Table);
    state.pop(1);
    state.get_global("saved_object");
    assert_eq!(state.typ(-1), LuaType::Table);
    state.pop(1);
}

#[test]
fn unparenthesized_string_call_is_supported() {
    let result = run_number(
        r#"
        local function wrap(s) return s end
        return wrap "a" .. "b" == "ab" and 1 or 0
    "#,
    );
    assert_eq!(result, 1.0);
}

#[test]
fn unparenthesized_table_call_is_supported() {
    let result = run_number(
        r#"
        local function get(t) return t.value end
        return get { value = 41 } + 1
    "#,
    );
    assert_eq!(result, 42.0);
}

#[test]
fn unparenthesized_method_call_is_supported() {
    let result = run_number(
        r#"
        local obj = {
            base = 9,
            plus = function(self, t) return self.base + t.delta end
        }
        return obj:plus { delta = 4 }
    "#,
    );
    assert_eq!(result, 13.0);
}

#[test]
fn table_constructor_accepts_identifier_array_entries() {
    let result = run_number(
        r#"
        local x = 7
        local t = {x, x + 1}
        return t[1] * 10 + t[2]
    "#,
    );
    assert_eq!(result, 78.0);
}

#[test]
fn table_constructor_distinguishes_named_fields_from_identifiers() {
    let result = run_number(
        r#"
        local x = 7
        local src = { value = 5 }
        local t = { x = 3, x, src.value }
        return t.x * 100 + t[1] * 10 + t[2]
    "#,
    );
    assert_eq!(result, 375.0);
}

#[test]
fn dotted_method_declaration_is_supported() {
    let result = run_number(
        r#"
        local mod = { sub = { base = 5 } }
        function mod.sub:add(x)
            return self.base + x
        end
        return mod.sub:add(7)
    "#,
    );
    assert_eq!(result, 12.0);
}

#[test]
fn error_msg_call_depth_shows_overflow() {
    let err = expect_error("local function r(n) return r(n+1) end\nr(0)");
    let msg = format!("{err}");
    assert!(
        msg.contains("call stack overflow") || msg.contains("depth"),
        "Call depth error should mention overflow/depth, got: {msg}"
    );
}
