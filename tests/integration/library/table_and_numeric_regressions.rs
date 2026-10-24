//! Behaviour and argument validation of the table library, `ipairs`, `next`,
//! `select`, `tonumber` and `math.log`.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, LuaType, RetCount, State};

use crate::support::{self, eval_number, run_error};

#[test]
fn ipairs_basic() {
    let sum = eval_number(
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
    let count = eval_number(
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
    let sum = eval_number(
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

#[test]
fn math_log_base_ten_uses_the_exact_base_ten_path() {
    assert_eq!(eval_number("return math.log(1000, 10)"), 3.0);
}

#[test]
fn table_insert_append() {
    let val = eval_number(
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
    let val = eval_number(
        r#"
        local t = {1, 2, 3}
        table.insert(t, 2, 99)
        return t[2]
    "#,
    );
    assert_eq!(val, 99.0);
}

#[test]
fn table_insert_rotation_preserves_order_across_storage_shapes() {
    // Each case walks `pairs` and builds "key=value key=value ..." over the
    // WHOLE table, so an element lost, duplicated or left in the wrong slot
    // anywhere in the middle fails - checking only the ends would not catch the
    // rotation carrying a value twice.
    let check = |code: &str, expected: &str| {
        let matched = eval_number(format!(
            r#"
            local t = {code}
            local parts = {{}}
            for key, value in pairs(t) do
                parts[#parts + 1] = key .. "=" .. tostring(value)
            end
            return table.concat(parts, " ") == "{expected}" and 1 or 0
        "#
        ));
        assert_eq!(matched, 1.0, "expected {expected} from {code}");
    };

    // Inline storage: four entries or fewer.
    check("{10, 20, 30} table.insert(t, 1, 99)", "1=99 2=10 3=20 4=30");
    // Map storage: promotion happens at the fifth entry.
    check(
        "{10, 20, 30, 40, 50} table.insert(t, 1, 99)",
        "1=99 2=10 3=20 4=30 5=40 6=50",
    );
    // Tombstoned slot, compacted before the rotation runs.
    check(
        "{10, 20, 30, 40, 50} t[5] = nil table.insert(t, 2, 99)",
        "1=10 2=99 3=20 4=30 5=40",
    );
    // Inserting in the middle rather than at either end.
    check(
        "{10, 20, 30, 40} table.insert(t, 3, 99)",
        "1=10 2=20 3=99 4=30 5=40",
    );
    // Appending through the positional form.
    check("{10, 20, 30} table.insert(t, 4, 99)", "1=10 2=20 3=30 4=99");
}

#[test]
fn table_sort_comparator_error_leaves_every_slot_untouched() {
    // The sort permutes a detached copy and only writes back on success, so a
    // comparator that fails part-way through must leave every original slot as
    // it was - not a partially sorted prefix.
    let mut state = State::new();
    support::load(
        &mut state,
        r#"
            t = {5, 3, 1, 4, 2}
            local n = 0
            table.sort(t, function(a, b)
                n = n + 1
                if n == 3 then error("stop") end
                return a < b
            end)
        "#,
    );
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("comparator error must propagate out of table.sort");

    support::eval(
        &mut state,
        r#"
            local parts = {}
            for i = 1, 5 do parts[#parts + 1] = tostring(t[i]) end
            return table.concat(parts, ",")
        "#,
        1,
    );
    assert_eq!(state.to_bytes(-1).unwrap(), b"5,3,1,4,2");
}

#[test]
fn table_insert_sparse_array_does_not_cache_a_non_border() {
    let value = eval_number(
        r#"
        local t = {1, 2, 3}
        t[5] = 5
        table.insert(t, 1, 99)
        return #t * 100 + t[4] * 10 + t[5]
    "#,
    );
    assert_eq!(value, 535.0);
}

#[test]
fn table_insert_nil_position_is_required() {
    let err = run_error("table.insert({}, nil, 9)");
    let ErrorKind::ArgError(arg) = err.kind() else {
        panic!("table.insert nil position must be an argument error: {err}");
    };
    assert_eq!(arg.arg_number(), 2);
    assert_eq!(arg.expected(), Some(LuaType::Number));
    assert_eq!(arg.received(), Some(LuaType::Nil));
}

#[test]
fn table_concat_rejects_non_integer_endpoints() {
    for (code, argument) in [
        ("return table.concat({}, '', 1.5)", 3),
        ("return table.concat({}, '', 1, 1e300)", 4),
    ] {
        let err = run_error(code);
        match err.kind() {
            ErrorKind::ArgError(error) => {
                assert_eq!(
                    error.to_string(),
                    format!(
                        "bad argument #{argument} to 'concat' (number has no integer representation)"
                    )
                );
            }
            kind => panic!("expected integer conversion error, got: {kind:?}"),
        }
    }
}

#[test]
fn tonumber_uses_lua_numeral_grammar() {
    let value = eval_number(
        r#"
        local a = tonumber(" \t+42\n")
        local b = tonumber(".5e1")
        local c = tonumber("0x10")
        local d = tonumber("-0X1.8p+1")
        local e = tonumber("10", nil)
        local f = tonumber("+ff", 16)
        if tonumber("nan") ~= nil or tonumber("NaN") ~= nil or tonumber("inf") ~= nil
            or tonumber("1_0") ~= nil or tonumber("0b10") ~= nil or tonumber("1e") ~= nil then
            return -1
        end
        return a + b + c + d + e + f
    "#,
    );
    assert_eq!(value, 325.0);
}

#[test]
fn tonumber_with_base_requires_a_string_first_argument() {
    let err = run_error("return tonumber(10, 16)");
    let ErrorKind::ArgError(arg) = err.kind() else {
        panic!("tonumber number with base must be an argument error: {err}");
    };
    assert_eq!(arg.arg_number(), 1);
    assert_eq!(arg.expected(), Some(LuaType::String));
    assert_eq!(arg.received(), Some(LuaType::Number));
}

#[test]
fn pairs_uses_builtin_next_after_rebinding() {
    let value = eval_number(
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
    let value = eval_number(
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
        let err = run_error(code);
        assert!(
            matches!(err.kind(), ErrorKind::ArgError(_)),
            "{code}: {err}"
        );
    }
    // A negative position is a valid integer that is simply out of range: it
    // must report "position out of bounds", not "no integer representation".
    for code in ["table.insert({1}, -1, 9)", "table.remove({1}, -1)"] {
        let err = run_error(code);
        assert!(
            err.to_string().contains("position out of bounds"),
            "{code}: {err}"
        );
    }
}

#[test]
fn table_insert_move_validate_before_mutation_and_random_draws_match_fresh_seeded_state() {
    for code in ["table.insert({})", "table.insert({}, 1, 2, 3)"] {
        let err = run_error(code);
        assert!(
            matches!(err.kind(), ErrorKind::LibraryError(_)),
            "{code}: {err}"
        );
    }
    let err = run_error("local t = {7}; table.move(t, 1, 1, 2, 42)");
    assert!(matches!(err.kind(), ErrorKind::ArgError(_)), "{err}");
    for code in [
        "math.random(0)",
        "math.random(2, 1)",
        "math.random(1, 2, 3)",
    ] {
        let err = run_error(code);
        assert!(
            matches!(err.kind(), ErrorKind::ArgError(_))
                || (code == "math.random(1, 2, 3)"
                    && matches!(err.kind(), ErrorKind::LibraryError(_))),
            "{code}: {err}"
        );
    }
    let mut first = State::new();
    let mut second = State::new();
    first.set_rng_seed(99);
    second.set_rng_seed(99);
    for state in [&mut first, &mut second] {
        support::eval(state, "return math.random() + math.random(1, 10)", 1);
    }
    assert_eq!(first.to_number(-1).unwrap(), second.to_number(-1).unwrap());
}

#[test]
fn table_remove_basic() {
    let val = eval_number(
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
    let val = eval_number(
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
    let val = eval_number(
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
    let val = eval_number(
        r#"
        local t = {1, 2, 3}
        table.sort(t, function(a, b) return a > b end)
        return t[1] * 100 + t[2] * 10 + t[3]
    "#,
    );
    assert_eq!(val, 321.0);
}

#[test]
fn table_sort_default_rejects_incomparable_values_without_mutation() {
    for (code, expected) in [
        (
            "return table.sort({1, 'a', 2})",
            "attempt to compare string with number",
        ),
        (
            "return table.sort({{}, {}})",
            "attempt to compare two table values",
        ),
        (
            "return table.sort({true, false})",
            "attempt to compare two boolean values",
        ),
    ] {
        let err = run_error(code);
        let ErrorKind::TypeError(kind) = err.kind() else {
            panic!("expected comparison type error: {err}");
        };
        assert_eq!(kind.to_string(), expected);
    }

    let singleton = eval_number("local t = {true}; table.sort(t); return #t");
    assert_eq!(singleton, 1.0);

    let mut state = State::new();
    support::eval(&mut state, "return {1, 'a', 2}", 1);
    host_table_sort(&mut state).expect_err("mixed default sort must fail");
    assert_eq!(state.get_top(), 1);
    state.push_number(1.0).unwrap();
    state.get_table(1).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 1.0);
    state.pop(1).unwrap();
    state.push_number(2.0).unwrap();
    state.get_table(1).unwrap();
    assert_eq!(state.to_string(-1).unwrap(), "a");
}

#[test]
fn table_sort_heap_comparator_is_bounded_and_reentrant() {
    let calls = eval_number(
        r#"
        local t, calls = {}, 0
        for i = 64, 1, -1 do t[#t + 1] = i end
        table.sort(t, function(a, b) calls = calls + 1; return a < b end)
        return calls
    "#,
    );
    assert!(calls < 1_024.0, "heap sort made too many calls: {calls}");

    let value = eval_number(
        r#"
        local t = {3, 1, 2}
        local other = {2, 1}
        table.sort(t, function(a, b)
            t.marker = 42
            table.sort(other)
            return a < b
        end)
        return t[1] * 100 + t[3] + t.marker + other[1]
    "#,
    );
    assert_eq!(value, 146.0);
}

#[test]
fn table_sort_preserves_entries_added_beyond_the_original_range() {
    let value = eval_number(
        r#"
        local t = {3, 1, 2}
        table.sort(t, function(a, b)
            t[4] = 40
            t[5] = 50
            return a < b
        end)
        return t[1] * 10000 + t[2] * 1000 + t[3] * 100 + t[4] + t[5]
    "#,
    );
    assert_eq!(value, 12390.0);
}

#[test]
fn table_sort_inconsistent_comparator_results_match_between_fresh_states() {
    let code = r#"
        local t, calls = {1, 2, 3, 4, 5}, 0
        table.sort(t, function(a, b) calls = calls + 1; return true end)
        return calls * 100000 + t[1] * 10000 + t[2] * 1000 + t[3] * 100 + t[4] * 10 + t[5]
    "#;
    assert_eq!(eval_number(code), eval_number(code));
}

#[test]
fn table_concat_basic() {
    let mut state = State::new();
    support::eval(&mut state, r#"return table.concat({1, 2, 3}, ", ")"#, 1);
    assert_eq!(state.to_string(-1).unwrap(), "1, 2, 3");
}

#[test]
fn table_concat_rejects_boolean_elements() {
    let err = run_error(r#"return table.concat({true}, ",")"#);
    assert!(
        matches!(err.kind(), ErrorKind::LibraryError(m)
            if m.to_string() == "invalid value (boolean) at index 1 in table for 'concat'"),
        "Expected the reference concat error, got: {err}"
    );
}

#[test]
fn table_concat_rejects_nil_elements_in_range() {
    let err = run_error(r#"return table.concat({1, nil, 3}, ",", 1, 3)"#);
    assert!(
        matches!(err.kind(), ErrorKind::LibraryError(m)
            if m.to_string() == "invalid value (nil) at index 2 in table for 'concat'"),
        "Expected the reference concat error, got: {err}"
    );
}

#[test]
fn table_unpack_basic() {
    let val = eval_number(
        r#"
        local a, b, c = table.unpack({10, 20, 30})
        return a + b + c
    "#,
    );
    assert_eq!(val, 60.0);
}

#[test]
fn global_unpack_supports_range() {
    let val = eval_number(
        r#"
        local a, b, c = unpack({10, 20, 30, 40}, 2, 4)
        return a + b + c
    "#,
    );
    assert_eq!(val, 90.0);
}

#[test]
fn table_move_overlapping_same_table_copies_backwards() {
    let val = eval_number(
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
    let val = eval_number(
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
    let val = eval_number(
        r#"
        local t = {1, 2, 3, 4, 5}
        table.move(t, 2, 4, 1)
        return t[1] * 10000 + t[2] * 1000 + t[3] * 100 + t[4] * 10 + t[5]
    "#,
    );
    assert_eq!(val, 23445.0);
}

#[test]
fn table_move_rejects_overflow_ranges_before_mutating() {
    let mut state = State::new();
    support::run(&mut state, "t = {10, 20, 30}");

    for (code, message) in [
        (
            "table.move(t, -1024, 9223372036854774784, 1)",
            "bad argument #3 to 'move' (too many elements to move)",
        ),
        (
            "table.move(t, 1, 2000, 9223372036854774784)",
            "bad argument #4 to 'move' (destination wrap around)",
        ),
    ] {
        support::load(&mut state, code);
        let err = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("overflow range must fail cleanly");
        assert_eq!(err.kind().to_string(), message);

        state.get_global("t").unwrap();
        for (index, expected) in [10.0, 20.0, 30.0].into_iter().enumerate() {
            state.push_number((index + 1) as f64).unwrap();
            state.get_table(-2).expect("table read succeeds");
            assert_eq!(
                state.to_number(-1).expect("table value is numeric"),
                expected
            );
            state.pop(1).unwrap();
        }
        state.pop(1).unwrap();
    }
}

#[test]
fn table_move_rejects_out_of_range_numbers_cleanly() {
    let error = run_error("table.move({}, -1e300, 1e300, 1)");
    assert!(matches!(error.kind(), ErrorKind::ArgError(_)));
    assert_eq!(
        error.kind().to_string(),
        "bad argument #2 to 'move' (number has no integer representation)"
    );
}

#[test]
fn select_negative_index_counts_from_end() {
    let val = eval_number(
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
    let err = run_error(r#"select(0, "a", "b")"#);
    match err.kind() {
        ErrorKind::ArgError(error) => {
            assert_eq!(
                error.to_string(),
                "bad argument #1 to 'select' (index out of range)"
            );
        }
        kind => panic!("Expected out-of-range argument error, got: {kind:?}"),
    }
}

#[test]
fn select_too_negative_index_errors() {
    let err = run_error(r#"select(-4, "a", "b", "c")"#);
    match err.kind() {
        ErrorKind::ArgError(error) => {
            assert_eq!(
                error.to_string(),
                "bad argument #1 to 'select' (index out of range)"
            );
        }
        kind => panic!("Expected out-of-range argument error, got: {kind:?}"),
    }
}

#[test]
fn tonumber_parses_base_argument() {
    let val = eval_number(
        r#"
        return tonumber("ff", 16) + tonumber("-10", 16) + tonumber("z", 36)
    "#,
    );
    assert_eq!(val, 274.0);
}

#[test]
fn tonumber_base_invalid_digit_returns_nil() {
    let val = eval_number(
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
    let err = run_error(r#"return tonumber("10", 37)"#);
    match err.kind() {
        ErrorKind::ArgError(error) => {
            assert_eq!(
                error.to_string(),
                "bad argument #2 to 'tonumber' (base out of range)"
            );
        }
        kind => panic!("Expected out-of-range argument error, got: {kind:?}"),
    }
}

#[test]
fn tonumber_base_requires_string_input() {
    let err = run_error(r#"return tonumber(10, 10)"#);
    assert!(
        matches!(err.kind(), ErrorKind::ArgError(_)),
        "Expected ArgError, got: {err}"
    );
}

/// Calls `table.sort` on the table at stack index 1, the way a host must now
/// that `State::table_sort` is crate-internal.
fn host_table_sort(state: &mut State) -> dellingr::Result<()> {
    state.get_global("table")?;
    state.push_string("sort")?;
    state.get_table(-2)?;
    state.remove(-2)?;
    state.push_value(1)?;
    state.call(ArgCount::Fixed(1), RetCount::Fixed(0))
}

fn assert_invalid_next_key(code: &str) {
    let err = run_error(code);
    match err.kind() {
        ErrorKind::LibraryError(error) => {
            assert_eq!(error.to_string(), "invalid key to 'next'");
        }
        kind => panic!("Expected invalid next key runtime error, got: {kind:?}"),
    }
}
