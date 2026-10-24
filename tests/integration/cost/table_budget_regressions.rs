//! `table.sort` and `table.move` charges: what they cost, and that an
//! exhausted budget stops them before (or partway through) mutation.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

use crate::support;

#[test]
fn table_sort_charges_before_mutating() {
    // At an exhausted budget the sort must be blocked BEFORE it mutates the
    // table or runs the comparator. table_sort charges its cost up front, so
    // with budget 0 it errors instead of sorting.
    let mut state = State::new();
    support::eval(&mut state, "return {3, 1, 2}", 1);

    state.set_cost_budget(0);
    let err = host_table_sort(&mut state).expect_err("exhausted budget must block the sort");
    assert!(matches!(err.kind(), ErrorKind::BudgetExceeded { .. }));
    assert_eq!(state.get_top(), 1);

    // Restore budget and confirm the table is untouched: still {3, 1, 2}.
    state.set_cost_budget(i64::MAX);
    state.push_number(1.0).unwrap();
    state.get_table(1).unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 3.0);
}

#[test]
fn table_move_costs_empty_and_each_moved_element() {
    for (code, expected_cost) in [
        ("return table.move({}, 1, 0, 1)", 2),
        ("return table.move({}, 1, 1, 1)", 2),
        ("return table.move({}, 1, 3, 1)", 4),
    ] {
        let mut state = State::new();
        support::eval(&mut state, code, 1);
        assert_eq!(state.cost_used(), expected_cost, "{code}");
    }

    let mut state = State::new();
    state.set_cost_budget(0);
    // Call the native function directly so bytecode dispatch cannot consume
    // the exhausted budget before table.move reaches its empty-range charge.
    state.get_global("table").unwrap();
    state.push_bytes("move").expect("short test string fits");
    state.get_table(-2).expect("table.move lookup succeeds");
    state.remove(-2).expect("table table is removed");
    state.new_table().unwrap();
    state.push_number(2.0).unwrap();
    state.push_number(1.0).unwrap();
    state.push_number(1.0).unwrap();
    let error = state
        .call(ArgCount::Fixed(4), RetCount::Fixed(1))
        .expect_err("empty table.move must charge a configured exhausted budget");
    assert!(matches!(error.kind(), ErrorKind::BudgetExceeded { .. }));
}

fn table_after_budgeted_move(budget: i64) -> (dellingr::error::Error, Vec<f64>) {
    let mut state = State::new();
    support::run(&mut state, "t = {1, 2, 3, 4, 5}");
    state.set_cost_budget(budget);
    support::load(&mut state, "table.move(t, 1, 4, 2)");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("limited move must stop at the exhausted budget");

    state.get_global("t").unwrap();
    let mut values = Vec::new();
    for index in 1..=5 {
        state.push_number(index as f64).unwrap();
        state.get_table(-2).expect("table read succeeds");
        values.push(state.to_number(-1).expect("table value is numeric"));
        state.pop(1).unwrap();
    }
    (error, values)
}

#[test]
fn table_move_budget_zero_does_not_mutate() {
    let (error, values) = table_after_budgeted_move(0);
    assert!(matches!(error.kind(), ErrorKind::BudgetExceeded { .. }));
    assert_eq!(values, [1.0, 2.0, 3.0, 4.0, 5.0]);
}

#[test]
fn table_move_budget_partial_mutation_is_deterministic() {
    let first = table_after_budgeted_move(2);
    let second = table_after_budgeted_move(2);
    assert!(matches!(first.0.kind(), ErrorKind::BudgetExceeded { .. }));
    assert!(matches!(second.0.kind(), ErrorKind::BudgetExceeded { .. }));
    assert_eq!(first.1, [1.0, 2.0, 3.0, 3.0, 4.0]);
    assert_eq!(first.1, second.1);
}

#[test]
fn table_move_cost_matches_between_fresh_states() {
    let run = || {
        let mut state = State::new();
        support::run(&mut state, "local t = {1, 2, 3}; table.move(t, 1, 3, 4)");
        state.cost_used()
    };
    assert_eq!(run(), run());
}

fn host_table_sort(state: &mut State) -> dellingr::Result<()> {
    state.get_global("table")?;
    state.push_string("sort")?;
    state.get_table(-2)?;
    state.remove(-2)?;
    state.push_value(1)?;
    state.call(ArgCount::Fixed(1), RetCount::Fixed(0))
}
