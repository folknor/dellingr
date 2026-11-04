//! The configurable cost model through the public API: weights per charge,
//! zero weights, `CostModel::zero` versus `Costing::Off`, the admission
//! granularity of per-unit charges under a weight above 1, and static
//! analysis under a model.

use dellingr::error::ErrorKind;
use dellingr::{Charge, CostModel, Costing, Program, State};

use crate::support;

fn metered(model: CostModel) -> State {
    State::builder().costing(Costing::Metered(model)).build()
}

/// What `source` charges on a fresh State metered by `model`.
fn cost_under(model: &CostModel, source: &str) -> u64 {
    let mut state = metered(model.clone());
    support::run(&mut state, source);
    state.cost_used()
}

/// A model in which only `charge` costs anything.
fn only(charge: Charge, weight: u32) -> CostModel {
    CostModel::zero().with(charge, weight)
}

fn is_budget_exceeded(result: &dellingr::Result<()>) -> bool {
    matches!(
        result.as_ref().map_err(dellingr::error::Error::kind),
        Err(ErrorKind::BudgetExceeded { .. })
    )
}

/// One source per charge that exercises it at least once.
const CASES: &[(Charge, &str)] = &[
    (Charge::Add, "local a = 3 return a + a"),
    (Charge::Sub, "local a = 3 return a - a"),
    (Charge::Mul, "local a = 3 return a * a"),
    (Charge::Div, "local a = 3 return a / a"),
    (Charge::Mod, "local a = 3 return a % a"),
    (Charge::Pow, "local a = 3 return a ^ a"),
    (Charge::Negate, "local a = 3 return -a"),
    (Charge::NewTable, "return {}"),
    (Charge::TableWrite, "local t = {} t.x = 1"),
    (Charge::ArrayElement, "return {1, 2}"),
    (Charge::Rawset, "rawset({}, 'k', 1)"),
    (Charge::MathAbs, "math.abs(1)"),
    (Charge::MathAcos, "math.acos(0.5)"),
    (Charge::MathAsin, "math.asin(0.5)"),
    (Charge::MathAtan, "math.atan(1)"),
    (Charge::MathAtan2, "math.atan2(1, 1)"),
    (Charge::MathCeil, "math.ceil(1.5)"),
    (Charge::MathCos, "math.cos(1)"),
    (Charge::MathDeg, "math.deg(1)"),
    (Charge::MathExp, "math.exp(1)"),
    (Charge::MathFloor, "math.floor(1.5)"),
    (Charge::MathFmod, "math.fmod(5, 3)"),
    (Charge::MathLog, "math.log(2)"),
    (Charge::MathMax, "math.max(1, 2)"),
    (Charge::MathMin, "math.min(1, 2)"),
    (Charge::MathModf, "math.modf(1.5)"),
    (Charge::MathRad, "math.rad(1)"),
    (Charge::MathRandom, "math.random()"),
    (Charge::MathSin, "math.sin(1)"),
    (Charge::MathSqrt, "math.sqrt(4)"),
    (Charge::MathTan, "math.tan(1)"),
    (Charge::StringFind, "string.find('abc', 'b')"),
    (Charge::StringFormat, "string.format('%s', 'x')"),
    (
        Charge::StringGmatch,
        "for _ in string.gmatch('ab', '%a') do end",
    ),
    (Charge::StringGsub, "string.gsub('abc', 'b', 'x')"),
    (Charge::StringLower, "string.lower('ABC')"),
    (Charge::StringMatch, "string.match('abc', 'b')"),
    (Charge::StringReverse, "string.reverse('abc')"),
    (Charge::StringSub, "string.sub('abc', 1, 2)"),
    (Charge::StringUpper, "string.upper('abc')"),
    (Charge::TableConcat, "table.concat({'a', 'b'}, ',')"),
    (Charge::TableInsert, "table.insert({}, 1)"),
    (Charge::TableMove, "table.move({1, 2}, 1, 2, 3)"),
    (Charge::TablePack, "table.pack(1, 2)"),
    (Charge::TableRemove, "table.remove({1, 2})"),
    (Charge::TableSort, "table.sort({2, 1})"),
];

#[test]
fn every_charge_has_a_case() {
    for &charge in Charge::ALL {
        assert!(
            CASES.iter().any(|&(case, _)| case == charge),
            "no case for {}",
            charge.name()
        );
    }
    assert_eq!(CASES.len(), Charge::ALL.len());
}

#[test]
fn every_charge_scales_linearly_with_its_weight() {
    for &(charge, source) in CASES {
        let unit = cost_under(&only(charge, 1), source);
        assert!(unit > 0, "{} charges nothing in {source}", charge.name());
        for weight in [0, 2, 5, 1000] {
            assert_eq!(
                cost_under(&only(charge, weight), source),
                unit * u64::from(weight),
                "{} at weight {weight} in {source}",
                charge.name()
            );
        }
    }
}

#[test]
fn the_default_model_weighs_every_charge_one() {
    let model = CostModel::default();
    let charges: Vec<Charge> = model.iter().map(|(charge, _)| charge).collect();
    assert_eq!(charges, Charge::ALL);
    for (charge, weight) in model.iter() {
        assert_eq!(weight, 1, "{}", charge.name());
        assert_eq!(model.weight(charge), 1);
        assert_eq!(CostModel::zero().weight(charge), 0);
    }
    assert_eq!(model, CostModel::default());
    assert_ne!(model, CostModel::zero());
}

#[test]
fn set_and_with_change_only_the_named_weight() {
    let mut model = CostModel::default();
    model.set(Charge::Pow, 9);
    assert_eq!(model, CostModel::default().with(Charge::Pow, 9));
    for (charge, weight) in model.iter() {
        assert_eq!(weight, if charge == Charge::Pow { 9 } else { 1 });
    }
}

#[test]
fn a_state_reports_the_costing_it_was_built_with() {
    assert_eq!(State::new().costing(), &Costing::default());
    assert_eq!(Costing::default(), Costing::Metered(CostModel::default()));
    let model = CostModel::default().with(Charge::Add, 4);
    assert_eq!(metered(model.clone()).costing(), &Costing::Metered(model));
    let off = State::builder().costing(Costing::Off).build();
    assert_eq!(off.costing(), &Costing::Off);
}

#[test]
fn the_default_model_charges_what_the_plain_constructor_charges() {
    let source = "local a = 3 local t = {a + a, -a} table.insert(t, string.upper('ab'))";
    assert_eq!(
        cost_under(&CostModel::default(), source),
        support::cost_of(source)
    );
}

#[test]
fn operator_weights_apply_to_register_and_constant_forms_alike() {
    // Register-register, register-constant and constant-register forms each
    // execute one instruction of the same charge.
    for (charge, operator) in [
        (Charge::Add, "+"),
        (Charge::Sub, "-"),
        (Charge::Mul, "*"),
        (Charge::Div, "/"),
        (Charge::Mod, "%"),
        (Charge::Pow, "^"),
    ] {
        let source = format!(
            "local a, b = 3, 4 local c = a {operator} b c = a {operator} 2 c = 2 {operator} a"
        );
        assert_eq!(
            cost_under(&only(charge, 7), &source),
            21,
            "{}",
            charge.name()
        );
        // Another operator's weight does not touch this source.
        let other = if charge == Charge::Add {
            Charge::Sub
        } else {
            Charge::Add
        };
        assert_eq!(cost_under(&only(other, 7), &source), 0, "{}", charge.name());
    }
}

#[test]
fn negation_weight_is_independent_of_subtraction() {
    let source = "local a = 3 local b = -a b = -a b = a - 1";
    assert_eq!(cost_under(&only(Charge::Negate, 5), source), 10);
    assert_eq!(cost_under(&only(Charge::Sub, 5), source), 5);
}

#[test]
fn table_constructor_weights_split_the_table_and_its_elements() {
    let source = "local t = {1, 2, 3}";
    assert_eq!(cost_under(&only(Charge::NewTable, 6), source), 6);
    assert_eq!(cost_under(&only(Charge::ArrayElement, 5), source), 15);
    assert_eq!(
        cost_under(
            &CostModel::zero()
                .with(Charge::NewTable, 2)
                .with(Charge::ArrayElement, 5),
            source
        ),
        17
    );
    assert_eq!(cost_under(&only(Charge::TableWrite, 5), source), 0);
}

#[test]
fn an_open_constructor_charges_the_live_element_count() {
    let source = "local function f() return 1, 2, 3, 4 end local t = {f()}";
    assert_eq!(cost_under(&only(Charge::ArrayElement, 3), source), 12);
}

#[test]
fn table_writes_are_weighted_per_write() {
    let source = "local t = {} t.x = 1 t[2] = 2 t.y = 3";
    assert_eq!(cost_under(&only(Charge::TableWrite, 4), source), 12);
}

#[test]
fn library_weights_scale_flat_and_per_byte_charges() {
    // string.upper charges its input bytes, at least 1.
    let model = only(Charge::StringUpper, 3);
    assert_eq!(cost_under(&model, "string.upper('')"), 3);
    assert_eq!(cost_under(&model, "string.upper('abcd')"), 12);
    assert_eq!(cost_under(&model, "string.upper('abcdefgh')"), 24);
    // string.sub charges its output bytes, at least 1.
    let model = only(Charge::StringSub, 4);
    assert_eq!(cost_under(&model, "string.sub('hello', 1, 3)"), 12);
    assert_eq!(cost_under(&model, "string.sub('hello', 1, 0)"), 4);
    // The flat charges stay flat however much they move.
    let model = only(Charge::TableInsert, 6);
    assert_eq!(cost_under(&model, "local t = {} table.insert(t, 1)"), 6);
    assert_eq!(
        cost_under(&model, "local t = {1, 2, 3, 4} table.insert(t, 1, 0)"),
        6
    );
    assert_eq!(cost_under(&only(Charge::MathSqrt, 9), "math.sqrt(4)"), 9);
    assert_eq!(
        cost_under(&only(Charge::Rawset, 8), "rawset({}, 'k', 1)"),
        8
    );
}

#[test]
fn a_library_weight_covers_only_its_own_function() {
    // The weight of string.match does not touch string.find, and the other
    // way round, including the pattern work each runs.
    let find = "string.find('hello world', 'o w')";
    let matched = "string.match('hello world', 'o w')";
    assert_eq!(cost_under(&only(Charge::StringMatch, 5), find), 0);
    assert_eq!(cost_under(&only(Charge::StringFind, 5), matched), 0);
    assert!(cost_under(&only(Charge::StringFind, 1), find) > 0);
    assert!(cost_under(&only(Charge::StringMatch, 1), matched) > 0);
}

#[test]
fn a_library_weight_covers_its_matcher_and_scan_work() {
    let short = "a".repeat(10);
    let long = "a".repeat(100);
    for (charge, function) in [(Charge::StringFind, "find"), (Charge::StringMatch, "match")] {
        let model = only(charge, 3);
        let cost =
            |subject: &str| cost_under(&model, &format!("string.{function}('{subject}', '%a+%d')"));
        let short_cost = cost(&short);
        let long_cost = cost(&long);
        // The matcher work grows with the subject, and all of it is weighted.
        assert!(long_cost > short_cost, "{function}");
        assert_eq!(short_cost % 3, 0, "{function}");
        assert_eq!(long_cost % 3, 0, "{function}");
        assert_eq!(
            long_cost / 3,
            cost_under(
                &only(charge, 1),
                &format!("string.{function}('{long}', '%a+%d')")
            ),
            "{function}"
        );
    }
    let model = only(Charge::StringGsub, 2);
    let gsub = |subject: &str| cost_under(&model, &format!("string.gsub('{subject}', '%a', 'x')"));
    assert!(gsub(&long) > gsub(&short));
    assert_eq!(gsub(&long) % 2, 0);
}

#[test]
fn zero_weights_make_work_free() {
    let model = CostModel::default().with(Charge::Add, 0);
    assert_eq!(cost_under(&model, "local a = 3 local b = a + a"), 0);
    // Other charges still apply.
    assert_eq!(cost_under(&model, "local a = 3 local b = a * a"), 1);
    let model = CostModel::default().with(Charge::StringFormat, 0);
    assert_eq!(
        cost_under(&model, "string.format('%s %s', 'long', 'text')"),
        0
    );
    assert_eq!(
        cost_under(&CostModel::zero(), "local t = {1, 2, 3} t.x = -1 + #t"),
        0
    );
}

#[test]
fn a_zero_weight_opcode_skips_the_budget_check() {
    let source = "local a = 0 for i = 1, 1000 do a = a + 1 end";

    // At an exhausted budget a weighted opcode fails and a free one does not.
    let mut weighted = State::new();
    weighted.set_cost_budget(10).expect("the State is metered");
    assert!(is_budget_exceeded(&support::try_run(&mut weighted, source)));

    let mut free = metered(CostModel::default().with(Charge::Add, 0));
    free.set_cost_budget(0).expect("the State is metered");
    support::try_run(&mut free, source).expect("a free opcode needs no budget");
    assert_eq!(free.cost_used(), 0);
}

#[test]
fn a_zero_weight_library_call_skips_the_budget_check() {
    let mut state = metered(only(Charge::MathSqrt, 0).with(Charge::Add, 1));
    state.set_cost_budget(0).expect("the State is metered");
    support::try_run(&mut state, "for i = 1, 100 do math.sqrt(i) end")
        .expect("a free library call needs no budget");
    assert_eq!(state.cost_used(), 0);
}

#[test]
fn a_weighted_opcode_that_crosses_the_budget_completes() {
    // Atomic charge: the first add costs 10 against a budget of 1 and
    // completes; the second is the one that fails.
    let mut state = metered(CostModel::default().with(Charge::Add, 10));
    state.set_cost_budget(1).expect("the State is metered");
    let result = support::try_run(&mut state, "local a = 0 for i = 1, 5 do a = a + 1 end");
    assert!(is_budget_exceeded(&result));
    assert_eq!(state.cost_used(), 10);
    assert!(state.cost_remaining() < 0);
}

#[test]
fn a_zero_model_keeps_host_charges_and_budgets() {
    let mut state = metered(CostModel::zero());
    state.set_cost_budget(5).expect("a zero model is metered");
    support::run(&mut state, "local t = {1, 2, 3} table.insert(t, 4)");
    assert_eq!(state.cost_used(), 0);
    assert_eq!(state.cost_remaining(), 5);

    state.consume_cost(3).expect("within budget");
    assert_eq!(state.cost_used(), 3);
    assert_eq!(state.cost_remaining(), 2);
    // Crossing completes, the next positive charge fails.
    state
        .consume_cost(3)
        .expect("the crossing charge completes");
    assert_eq!(state.cost_used(), 6);
    let error = state.consume_cost(1).expect_err("the budget is spent");
    assert!(matches!(error.kind(), ErrorKind::BudgetExceeded { .. }));
    // A zero charge is never blocked.
    state.consume_cost(0).expect("zero cost is free");
}

#[test]
fn host_charges_are_raw_under_any_weights() {
    let mut state = metered(CostModel::default().with(Charge::Add, 9));
    state.consume_cost(3).expect("no budget is set");
    assert_eq!(state.cost_used(), 3);

    let mut state = metered(CostModel::zero());
    state.consume_cost(3).expect("no budget is set");
    assert_eq!(state.cost_used(), 3);
}

#[test]
fn off_admits_everything_and_counts_nothing() {
    let mut state = State::builder().costing(Costing::Off).build();
    support::run(
        &mut state,
        "local a = 0 local t = {} for i = 1, 1000 do a = a + i * 2 t[i] = string.upper('abc') end",
    );
    assert_eq!(state.cost_used(), 0);
    assert_eq!(state.cost_remaining(), i64::MAX);

    state
        .consume_cost(u64::MAX)
        .expect("Off admits host charges");
    state.consume_cost(u64::MAX).expect("again");
    assert_eq!(state.cost_used(), 0);
    assert_eq!(state.cost_remaining(), i64::MAX);
}

#[test]
fn off_refuses_a_budget_and_changes_nothing() {
    let mut state = State::builder().costing(Costing::Off).build();
    for budget in [0, 1, i64::MAX, -5] {
        let error = state
            .set_cost_budget(budget)
            .expect_err("Off refuses every budget");
        assert!(matches!(error.kind(), ErrorKind::CostingOff), "{budget}");
    }
    assert_eq!(state.cost_remaining(), i64::MAX);
    assert_eq!(state.cost_used(), 0);
    // A refused budget leaves the State usable and unbudgeted.
    support::run(&mut state, "local a = 0 for i = 1, 10000 do a = a + 1 end");
    assert_eq!(state.cost_used(), 0);
    assert_eq!(state.costing(), &Costing::Off);
}

#[test]
fn a_refused_budget_does_not_reset_a_metered_state() {
    // The refusal belongs to Off only: a metered State accepts any budget.
    let mut state = State::new();
    support::run(&mut state, "local a = 3 local b = a + a");
    assert_eq!(state.cost_used(), 1);
    state.set_cost_budget(100).expect("metered");
    assert_eq!(state.cost_used(), 0);
    assert_eq!(state.cost_remaining(), 100);
}

#[test]
fn table_move_stops_at_the_first_element_that_starts_with_no_budget() {
    // Each copied element is a charge unit of weight 3. The element in flight
    // when the budget crosses zero completes; the next one never starts.
    fn after_move(budget: i64) -> (bool, Vec<f64>, u64) {
        let mut state = metered(only(Charge::TableMove, 3));
        support::run(&mut state, "t = {1, 2, 3, 4, 5}");
        state.set_cost_budget(budget).expect("metered");
        let result = support::try_run(&mut state, "table.move(t, 1, 4, 2)");
        let mut values = Vec::new();
        for index in 1..=5 {
            state.get_global("t").unwrap();
            state.push_number(f64::from(index)).unwrap();
            state.get_table(-2).unwrap();
            values.push(state.to_number(-1).unwrap());
            state.pop(2).unwrap();
        }
        (is_budget_exceeded(&result), values, state.cost_used())
    }

    // The move copies backwards: t[5], t[4], t[3], t[2].
    assert_eq!(after_move(0), (true, vec![1.0, 2.0, 3.0, 4.0, 5.0], 0));
    assert_eq!(after_move(1), (true, vec![1.0, 2.0, 3.0, 4.0, 4.0], 3));
    assert_eq!(after_move(3), (true, vec![1.0, 2.0, 3.0, 4.0, 4.0], 3));
    assert_eq!(after_move(4), (true, vec![1.0, 2.0, 3.0, 3.0, 4.0], 6));
    assert_eq!(after_move(7), (true, vec![1.0, 2.0, 2.0, 3.0, 4.0], 9));
    // Enough for all four elements: the last one starts with budget left.
    assert_eq!(after_move(10), (false, vec![1.0, 1.0, 2.0, 3.0, 4.0], 12));
}

#[test]
fn scans_and_matchers_stop_mid_way_at_a_weight_above_one() {
    let subject = "a".repeat(200);
    for source in [
        format!("string.find('{subject}', 'b')"),
        format!("string.match('{subject}', '%a+%d')"),
    ] {
        let model = only(
            if source.starts_with("string.find") {
                Charge::StringFind
            } else {
                Charge::StringMatch
            },
            3,
        );
        let full = cost_under(&model, &source);
        let budget = i64::try_from(full / 2).expect("a small cost");

        let mut state = metered(model);
        state.set_cost_budget(budget).expect("metered");
        let result = support::try_run(&mut state, &source);
        assert!(is_budget_exceeded(&result), "{source}");
        // Stopped part way, not after the whole scan.
        assert!(state.cost_used() < full, "{source}");
        // Per-unit admission: the last unit started with budget left, so the
        // overshoot is under one unit's weight.
        let used = i64::try_from(state.cost_used()).expect("a small cost");
        assert!(used >= budget && used - budget < 3, "{source}: {used}");
    }
}

#[test]
fn analysis_applies_the_model_to_every_scope() {
    let program = Program::compile(
        "local a = 3\n\
         local b = a + a\n\
         b = a * a\n\
         b = -a\n\
         local t = {1, 2, 3}\n\
         local function f(x) return x + x end\n",
        None,
    )
    .expect("the program compiles");

    let default = program.analyze_cost();
    assert_eq!(
        program
            .analyze_cost_with(&CostModel::default())
            .root
            .total_cost,
        default.root.total_cost
    );
    assert_eq!(
        program
            .analyze_cost_with(&CostModel::zero())
            .root
            .total_cost,
        0
    );

    let model = CostModel::default()
        .with(Charge::Add, 10)
        .with(Charge::Mul, 0)
        .with(Charge::Negate, 2)
        .with(Charge::NewTable, 3)
        .with(Charge::ArrayElement, 4);
    let weighted = program.analyze_cost_with(&model);
    // One add, a free multiply, a negation, a constructor with 3 elements.
    assert_eq!(weighted.root.own_cost, 10 + 2 + 3 + 12);
    assert_eq!(weighted.root.nested.len(), 1);
    assert_eq!(weighted.root.nested[0].own_cost, 10);
    assert_eq!(weighted.root.total_cost, 27 + 10);

    // Counts describe the program, not the model.
    assert_eq!(
        weighted.root.arithmetic_ops(),
        default.root.arithmetic_ops()
    );
    assert_eq!(weighted.root.negations(), default.root.negations());
    assert_eq!(weighted.root.array_elements(), 3);
    assert_eq!(weighted.root.instructions, default.root.instructions);
}

#[test]
fn zero_weights_keep_scope_operation_counts_visible() {
    let program = Program::compile(
        "local a = 1\n\
         local b = 2\n\
         local c = a + b\n\
         local t = {named = c, [b] = c}\n\
         t.extra = c",
        None,
    )
    .expect("the program compiles");
    let analysis = program.analyze_cost_with(&CostModel::zero());
    assert_eq!(analysis.root.own_cost, 0);
    assert_eq!(analysis.root.arithmetic_ops(), 1);
    assert_eq!(analysis.root.table_writes(), 3);

    let rendered = analysis.to_string();
    let (_, scopes) = rendered
        .split_once("--- Per-Scope Breakdown ---")
        .expect("the report has a per-scope section");
    assert!(
        scopes.contains("main: cost 0\n  arithmetic: 1\n"),
        "{rendered}"
    );
    assert!(scopes.contains("table writes: 3"), "{rendered}");
    assert!(!scopes.contains("cost 0 (static)"), "{rendered}");
}

#[test]
fn scope_counts_cover_only_their_chunk_and_open_array_batches_count_zero() {
    let program = Program::compile(
        "local t = {1, 2, f()}\n\
         local keyed = {named = 1, [2] = 3}\n\
         keyed.extra = 4\n\
         local function inner()\n\
             local a = 1\n\
             local b = 2\n\
             return a + b\n\
         end",
        None,
    )
    .expect("the program compiles");
    let analysis = program.analyze_cost();
    let nested = &analysis.root.nested[0];

    assert_eq!(analysis.root.array_elements(), 0);
    assert_eq!(analysis.root.table_writes(), 3);
    assert_eq!(analysis.root.arithmetic_ops(), 0);
    assert_eq!(nested.arithmetic_ops(), 1);
    assert_eq!(analysis.root.function_calls(), 1);
    assert_eq!(nested.function_calls(), 0);
    assert!(analysis.root.instructions > 0);
    assert!(nested.instructions > 0);

    let rendered = analysis.to_string();
    let total = rendered
        .lines()
        .find_map(|line| line.strip_prefix("  Total instructions:   "))
        .expect("the report includes the all-scope instruction total")
        .parse::<u64>()
        .expect("the instruction total is numeric");
    assert_eq!(total, analysis.root.instructions + nested.instructions);
}

#[test]
fn analysis_agrees_with_a_straight_line_run() {
    // With no loops, branches or native calls, the static total is the cost.
    let source = "local a = 3 local b = a + a b = a * a b = -a local t = {1, 2, 3}";
    let program = Program::compile(source, None).expect("the program compiles");
    for model in [
        CostModel::default(),
        CostModel::zero(),
        CostModel::default()
            .with(Charge::Add, 10)
            .with(Charge::Negate, 2)
            .with(Charge::ArrayElement, 4),
    ] {
        assert_eq!(
            program.analyze_cost_with(&model).root.total_cost,
            cost_under(&model, source)
        );
    }
}
