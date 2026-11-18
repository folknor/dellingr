//! Numeric `for` execution: ascending, stepped, descending and empty ranges,
//! and the typed loop region against canonical dispatch.

use dellingr::{ArgCount, Charge, CostModel, Costing, RetCount, State};

use crate::support;
use crate::support::eval_number;

/// A loop whose body is straight-line arithmetic, a move and a literal load,
/// so it runs as a typed region, with a closure taken before the loop that
/// reads the captured locals after a failure. `{twin}` is spliced into the
/// body: a free upvalue read there disqualifies the region without changing
/// any charge, giving the canonical reference.
fn region_program(twin: &str) -> String {
    format!(
        r#"
        local up = 0
        function run()
            local sum, last, k = 0.5, 0, 0
            peek = function() return sum, last, k end
            for i = 1, 40 do
                sum = sum + i * 2.5 - i / 3.0
                last = i
                k = 7
                sum = -sum
                {twin}
            end
            return sum
        end
        "#
    )
}

/// Outcome of `run()` under `budget`: whether it completed, the meter, and
/// what the captured locals hold afterwards.
fn region_outcome(model: &CostModel, source: &str, budget: i64) -> (bool, u64, [f64; 3]) {
    let mut state = State::builder()
        .costing(Costing::Metered(model.clone()))
        .build();
    support::run(&mut state, source);
    state.set_cost_budget(budget).expect("the State is metered");
    state.get_global("run").expect("run is defined");
    let completed = match state.call(ArgCount::Fixed(0), RetCount::Fixed(1)) {
        Ok(_) => true,
        Err(error) => {
            assert!(
                matches!(
                    error.kind(),
                    dellingr::error::ErrorKind::BudgetExceeded { .. }
                ),
                "budget {budget} returned {error:?}"
            );
            false
        }
    };
    let used = state.cost_used();
    state
        .set_cost_budget(i64::MAX)
        .expect("the State is metered");
    state.get_global("peek").expect("peek is defined");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(3))
        .expect("peek runs unmetered");
    let values = [
        state.to_number(-3).expect("sum is a number"),
        state.to_number(-2).expect("last is a number"),
        state.to_number(-1).expect("k is a number"),
    ];
    (completed, used, values)
}

#[test]
fn numeric_loop_region_matches_canonical_dispatch_at_every_budget() {
    let region = region_program("");
    let canonical = region_program("local _u = up");
    let models = [
        CostModel::default(),
        CostModel::default()
            .with(Charge::Mul, 0)
            .with(Charge::Sub, 5)
            .with(Charge::Negate, 3),
        CostModel::zero(),
    ];
    for model in &models {
        for budget in -1..=600 {
            assert_eq!(
                region_outcome(model, &region, budget),
                region_outcome(model, &canonical, budget),
                "budget {budget} under {model:?}"
            );
        }
    }
}

#[test]
fn numeric_loop_region_falls_back_when_a_read_is_not_a_number() {
    let error = support::run_error(
        r#"
        local sum = "x"
        for i = 1, 3 do
            sum = sum * 2
        end
        "#,
    );
    assert!(
        error
            .to_string()
            .contains("attempt to perform arithmetic on a string value"),
        "{error}"
    );
}

#[test]
fn numeric_for_loop_basic() {
    let sum = eval_number(
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
    let sum = eval_number(
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
    let sum = eval_number(
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
    let count = eval_number(
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
