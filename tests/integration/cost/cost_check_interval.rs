//! The opcode batching interval must not change budget outcomes.

use dellingr::error::ErrorKind;
use dellingr::limits::COST_CHECK_INTERVAL;
use dellingr::{ArgCount, RetCount, State};

use crate::support;

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
    // Each statement emits one charged addition; global reads and writes are
    // free. These sizes put charges immediately before, at, and after interval
    // boundaries, including a second batch.
    let interval = usize::try_from(COST_CHECK_INTERVAL).expect("cost interval fits in usize");
    for additions in [
        1,
        2,
        interval - 1,
        interval,
        interval + 1,
        2 * interval - 1,
        2 * interval,
        2 * interval + 1,
    ] {
        let source = arithmetic_program(additions);

        let mut count_only = State::new();
        support::eval(&mut count_only, &source, 1);
        assert_eq!(
            count_only.cost_used(),
            u64::try_from(additions).expect("test program size fits in u64")
        );

        let max_budget = i64::try_from(additions).expect("test program size fits in i64") + 1;
        for budget in -1..=max_budget {
            let (should_complete, expected_used) = immediate_flush_reference(budget, additions);
            let mut state = State::new();
            state.set_cost_budget(budget);
            support::load(&mut state, &source);

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

/// Keep the sweep bounded in charged work, including stress examples. Compile
/// each example once and reuse its Program in both modes and every budget.
#[cfg(feature = "cost-check-interval")]
#[test]
fn corpus_cost_check_interval_differential() {
    use crate::support::PushNewRustFn;
    use dellingr::{Program, with_cost_check_interval};

    fn execute(
        program: &Program,
        budget: i64,
        natural: Option<support::error_boundary_cases::Setup>,
    ) -> (Vec<Vec<u8>>, u64, Option<String>) {
        let (mut state, recorder) = support::Recorder::state();
        if let Some(setup) = natural {
            // The natural cases' callbacks, as `error_boundary_cases::run`
            // registers them; that runner rejects non-budget errors under a
            // budget, so the natural rows execute here.
            let callbacks: [(&str, dellingr::RustFunc); 3] = [
                ("fail", support::error_boundary_cases::fail),
                ("overreport", support::error_boundary_cases::overreport),
                ("oversized", support::error_boundary_cases::oversized),
            ];
            for (name, callback) in callbacks {
                state.push_new_rust_fn(callback).unwrap();
                state.set_global(name).unwrap();
            }
            setup.install(&mut state);
        }
        state.load(program).unwrap();
        state.set_cost_budget(budget);
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .err()
            .map(|error| error.to_string());
        (recorder.prints(), state.cost_used(), error)
    }

    let interval = i64::try_from(COST_CHECK_INTERVAL).expect("interval fits in i64");
    let budgets = [
        -1,
        0,
        1,
        2,
        3,
        7,
        interval - 1,
        interval,
        interval + 1,
        2 * interval - 1,
        2 * interval,
        2 * interval + 1,
        4 * interval + 1,
    ];
    for example in support::examples::EXAMPLES {
        let source = std::fs::read(example.path).unwrap();
        let program = Program::compile(&source, Some(b"interval-corpus")).unwrap();
        for budget in budgets {
            let immediate = with_cost_check_interval(1, || execute(&program, budget, None));
            let batched =
                with_cost_check_interval(COST_CHECK_INTERVAL, || execute(&program, budget, None));
            assert_eq!(immediate, batched, "{} budget={budget}", example.relative);
        }
    }

    // The shared boundary runner also checks surviving operands, handler
    // writes, notification counts and reuse after each failed call. These
    // cases are small, so walk every prefix up to completion, as the boundary
    // sweep does, rather than only the interval edges.
    for &(name, source) in support::error_boundary_cases::CASES {
        let complete = support::error_boundary_cases::run(name, source, None, None).cost;
        let complete = i64::try_from(complete).expect("boundary case cost fits in i64");
        for budget in -1..=complete + 1 {
            let run = || {
                let outcome = support::error_boundary_cases::run(name, source, Some(budget), None);
                (outcome.cost, outcome.transcript)
            };
            assert_eq!(
                with_cost_check_interval(1, run),
                with_cost_check_interval(COST_CHECK_INTERVAL, run),
                "boundary {name} budget={budget}"
            );
        }
    }
    for (name, source, setup) in support::error_boundary_cases::natural() {
        let program = Program::compile(&source, Some(b"interval-natural")).unwrap();
        // Include unbounded execution so each natural failure actually occurs;
        // small budgets also exercise the prefixes that stop before it.
        for budget in budgets.into_iter().chain([i64::MAX]) {
            let run = || execute(&program, budget, Some(setup));
            assert_eq!(
                with_cost_check_interval(1, run),
                with_cost_check_interval(COST_CHECK_INTERVAL, run),
                "natural failure {name} budget={budget}"
            );
        }
    }
}
