//! The opcode batching interval must not change budget outcomes.

use dellingr::error::ErrorKind;
use dellingr::{ArgCount, RetCount, State};

/// Reference behavior when each unit-cost opcode flushes immediately.
fn immediate_flush_reference(budget: i64, additions: usize) -> (bool, u64) {
    let mut remaining = budget;
    let mut used = 0_u64;

    for _ in 0..additions {
        if remaining <= 0 {
            return (false, used);
        }
        remaining -= 1;
        used += 1;
    }

    (true, used)
}

fn arithmetic_program(additions: usize) -> String {
    let mut source = String::from("interval_value = 0\n");
    for _ in 0..additions {
        source.push_str("interval_value = interval_value + 1\n");
    }
    source.push_str("return interval_value");
    source
}

#[test]
fn opcode_cost_interval_matches_immediate_flush_across_budgets() {
    // Each statement emits one charged OP_ADD; global reads and writes are
    // free. These sizes put charges immediately before, at, and after interval
    // boundaries, including a second batch.
    for additions in [1, 2, 63, 64, 65, 127, 128, 129] {
        let source = arithmetic_program(additions);

        let mut count_only = State::new();
        count_only
            .load_string(&source)
            .expect("arithmetic test program compiles");
        count_only
            .call(ArgCount::Fixed(0), RetCount::Fixed(1))
            .expect("arithmetic test program completes without a budget");
        assert_eq!(
            count_only.cost_used(),
            u64::try_from(additions).expect("test program size fits in u64")
        );

        let max_budget = i64::try_from(additions).expect("test program size fits in i64") + 1;
        for budget in -1..=max_budget {
            let (should_complete, expected_used) = immediate_flush_reference(budget, additions);
            let mut state = State::new();
            state.set_cost_budget(budget);
            state
                .load_string(&source)
                .expect("arithmetic test program compiles");

            let call = state.call(ArgCount::Fixed(0), RetCount::Fixed(1));
            if should_complete {
                call.expect("reference says the program fits its budget");
            } else {
                let error = call.expect_err("reference says the next addition is blocked");
                assert!(
                    matches!(error.kind(), ErrorKind::BudgetExceeded { .. }),
                    "budget {budget} returned {error:?}"
                );
            }

            assert_eq!(state.cost_used(), expected_used, "budget {budget}");
            state
                .get_global("interval_value")
                .expect("global value remains readable after the call");
            let actual_value = state
                .to_number(-1)
                .expect("global arithmetic result remains numeric");
            assert_eq!(
                actual_value,
                f64::from(u32::try_from(expected_used).expect("test cost fits in u32")),
                "budget {budget}"
            );
        }
    }
}
