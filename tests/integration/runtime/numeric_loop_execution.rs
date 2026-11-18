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

/// A loop reading fields of an upvalue table and of a local table it never
/// writes, besides arithmetic, so it runs as a region with invariant reads.
fn field_region_program(twin: &str) -> String {
    format!(
        r#"
        local up = 0
        local cfg = {{ rate = 1.5, bias = -2 }}
        function run()
            local sum, last, k = 0.5, 0, 0
            peek = function() return sum, last, k end
            local obj = {{ gain = 0.25 }}
            for i = 1, 40 do
                sum = sum + cfg.rate * i - obj.gain
                last = cfg.bias
                k = sum * obj.gain
                {twin}
            end
            return sum
        end
        "#
    )
}

#[test]
fn numeric_loop_region_with_field_reads_matches_canonical_dispatch_at_every_budget() {
    let region = field_region_program("");
    let canonical = field_region_program("local _u = up");
    let models = [
        CostModel::default(),
        CostModel::default()
            .with(Charge::Mul, 0)
            .with(Charge::Sub, 5),
        CostModel::zero(),
    ];
    for model in &models {
        for budget in -1..=400 {
            assert_eq!(
                region_outcome(model, &region, budget),
                region_outcome(model, &canonical, budget),
                "budget {budget} under {model:?}"
            );
        }
    }
}

#[test]
fn numeric_loop_region_field_reads_keep_canonical_semantics() {
    // An inherited field resolves through `__index`; a string field fails.
    let sum = eval_number(
        r#"
        local base = { step = 2 }
        local t = setmetatable({}, { __index = base })
        local sum = 0
        for i = 1, 5 do
            sum = sum + t.step
        end
        return sum
    "#,
    );
    assert_eq!(sum, 10.0);
    let error = support::run_error(
        r#"
        local t = { step = "x" }
        local sum = 0
        for i = 1, 3 do
            sum = sum + t.step
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

/// A loop with forward branches, a write on one arm only, and indexed reads
/// of an upvalue table and of a local table it never writes. `ys` misses
/// past 25 and answers through `__index`, which exits the region mid-body
/// to canonical dispatch.
fn control_region_program(twin: &str) -> String {
    format!(
        r#"
        local up = 0
        local xs = {{}}
        for i = 1, 30 do xs[i] = i * 1.5 end
        function run()
            local sum, last, k = 0.5, 0, 0
            peek = function() return sum, last, k end
            local ys = setmetatable({{}}, {{ __index = function(_, key) return key * 2 end }})
            for i = 1, 25 do ys[i] = 31 - i end
            for i = 1, 40 do
                local a = xs[i % 30 + 1]
                local b = ys[i]
                if a < b or b == 7 then
                    sum = sum + a
                else
                    last = b
                end
                if a ~= 3 then k = k + 1 end
                sum = -sum
                {twin}
            end
            return sum
        end
        "#
    )
}

#[test]
fn numeric_loop_region_with_branches_matches_canonical_dispatch_at_every_budget() {
    let region = control_region_program("");
    let canonical = control_region_program("local _u = up");
    let models = [
        CostModel::default(),
        CostModel::default()
            .with(Charge::Add, 0)
            .with(Charge::Mod, 5)
            .with(Charge::Negate, 3),
        CostModel::zero(),
    ];
    for model in &models {
        for budget in -1..=900 {
            assert_eq!(
                region_outcome(model, &region, budget),
                region_outcome(model, &canonical, budget),
                "budget {budget} under {model:?}"
            );
        }
    }
}

/// A caller looping over a numeric helper. `{twin}` is spliced at the top of
/// the helper: a free boolean load there keeps it off the leaf path without
/// changing any charge. (An upvalue read would not: a leaf takes one as a
/// field receiver it never uses.)
fn leaf_program(twin: &str) -> String {
    format!(
        r#"
        local up = 0
        local function step(x, y)
            {twin}
            local t = x * 2.5 - y
            return -t / 3
        end
        function run()
            local sum, last, k = 0.5, 0, 0
            peek = function() return sum, last, k end
            for i = 1, 40 do
                sum = step(sum, i)
                last = i
                k = step(i, 1)
            end
            return sum
        end
        "#
    )
}

#[test]
fn numeric_leaf_matches_canonical_call_at_every_budget() {
    let leaf = leaf_program("");
    let canonical = leaf_program("local _u = false");
    let models = [
        CostModel::default(),
        CostModel::default()
            .with(Charge::Mul, 0)
            .with(Charge::Sub, 5)
            .with(Charge::Negate, 3),
        CostModel::zero(),
    ];
    for model in &models {
        for budget in -1..=500 {
            assert_eq!(
                region_outcome(model, &leaf, budget),
                region_outcome(model, &canonical, budget),
                "budget {budget} under {model:?}"
            );
        }
    }
}

/// A typed loop calling global numeric leaves, one with a temporary above
/// its function register, so the region runs across the calls. `{twin}` is
/// spliced into one helper: a free boolean load makes it no leaf, which
/// keeps the whole loop canonical without changing any charge.
fn global_call_program(twin: &str) -> String {
    format!(
        r#"
        local up = 0
        function scale(x, y)
            {twin}
            return x * 0.5 + y
        end
        function shift(x) return x - 3 end
        function run()
            local sum, last, k = 0.5, 0, 0
            peek = function() return sum, last, k end
            for i = 1, 40 do
                last = i * 2
                sum = scale(sum, i * 3 - 1)
                k = shift(sum) / 2
            end
            return sum
        end
        "#
    )
}

#[test]
fn numeric_loop_region_with_leaf_calls_matches_canonical_dispatch_at_every_budget() {
    let region = global_call_program("");
    let canonical = global_call_program("local _u = false");
    let models = [
        CostModel::default(),
        CostModel::default()
            .with(Charge::Mul, 0)
            .with(Charge::Sub, 5)
            .with(Charge::Add, 3),
        CostModel::zero(),
    ];
    for model in &models {
        for budget in -1..=700 {
            assert_eq!(
                region_outcome(model, &region, budget),
                region_outcome(model, &canonical, budget),
                "budget {budget} under {model:?}"
            );
        }
    }
}

#[test]
fn numeric_loop_region_calls_see_the_callee_the_loop_started_with() {
    // A callee that is not a leaf, or that is rebound before the loop, runs
    // canonically; a non-function callee fails as a call does.
    let n = eval_number(
        r#"
        function f(x) return x + 1 end
        local sum = 0
        for i = 1, 3 do sum = f(sum) end
        f = function(x) return x + 10 end
        for i = 1, 3 do sum = f(sum) end
        return sum
    "#,
    );
    assert_eq!(n, 33.0);
    let error = support::run_error(
        r#"
        g = 5
        local sum = 0
        for i = 1, 3 do sum = g(sum) end
        "#,
    );
    assert!(error.to_string().contains("attempt to call"), "{error}");
}

#[test]
fn numeric_leaf_declines_arguments_that_are_not_numbers() {
    let error = support::run_error(
        r#"
        local function inc(x) return x + 1 end
        inc("x")
        "#,
    );
    assert!(
        error
            .to_string()
            .contains("attempt to perform arithmetic on a string value"),
        "{error}"
    );
    let error = support::run_error(
        r#"
        local function inc(x) return x + 1 end
        inc()
        "#,
    );
    assert!(
        error.to_string().contains("arithmetic on a nil value"),
        "{error}"
    );
    // `return x` reads `x`: a non-number passes through canonically.
    let n = eval_number(
        r#"
        local function id(x) return x end
        local function two(a, b) return a + b end
        local s = id("abc")
        local a, b = two(1, 2), two(3, 4, 5)
        if s ~= "abc" or id() ~= nil then return -1 end
        return a + b
    "#,
    );
    assert_eq!(n, 10.0);
}

/// Methods and helpers that read and store fields of a parameter and of an
/// upvalue table, one returning nothing, called from a loop that is no
/// region. `{twin}` is spliced at the top of each: a free boolean load keeps
/// them off the leaf path without changing any charge.
fn field_leaf_program(twin: &str) -> String {
    format!(
        r#"
        local stats = {{ calls = 0 }}
        local Counter = {{}}
        Counter.__index = Counter
        function Counter:inc(n)
            {twin}
            stats.calls = stats.calls + 1
            self.count = self.count + n * 2
            return self.count
        end
        function Counter:damage(n)
            {twin}
            self.hp = self.hp - n
        end
        local function area(s)
            {twin}
            return s.w * s.h
        end
        local function bump(t)
            {twin}
            stats.calls = stats.calls + 1
            t.calls = t.calls + 10
            return stats.calls
        end
        local c = setmetatable({{ count = 0.5, hp = 100 }}, Counter)
        local sh = {{ w = 2, h = 3 }}
        local k = 0
        peek = function() return c.count + stats.calls, c.hp, k end
        function run()
            local sum = 0
            for i = 1, 30 do
                sum = c:inc(i)
                c:damage(i % 3)
                k = k + area(sh) + bump(stats)
            end
            return sum
        end
        "#
    )
}

#[test]
fn field_leaf_matches_canonical_call_at_every_budget() {
    let leaf = field_leaf_program("");
    let canonical = field_leaf_program("local _u = false");
    let models = [
        CostModel::default(),
        CostModel::default()
            .with(Charge::TableWrite, 4)
            .with(Charge::Mul, 0)
            .with(Charge::Sub, 5),
        CostModel::zero(),
    ];
    for model in &models {
        for budget in -1..=900 {
            assert_eq!(
                region_outcome(model, &leaf, budget),
                region_outcome(model, &canonical, budget),
                "budget {budget} under {model:?}"
            );
        }
    }
}

#[test]
fn field_leaf_declines_to_canonical_semantics() {
    // A read through `__index`, a store of a new key through `__newindex`,
    // aliased receivers, a zero-result call adjusted to fixed and open
    // results, and a receiver whose register a later number overwrites.
    let n = eval_number(
        r##"
        local log = 0
        local base = { step = 2 }
        local mt = { __index = base, __newindex = function(t, k, v) log = log + v end }
        local function add(t, n) t.step = t.step + n return t.step end
        local inherited = setmetatable({}, mt)
        local first = add(inherited, 1)
        if first ~= 2 or log ~= 3 or rawget(inherited, "step") ~= nil then return -1 end
        local own = setmetatable({ step = 5 }, mt)
        if add(own, 1) ~= 6 or log ~= 3 then return -2 end
        local g = { n = 1 }
        local function both(t) g.n = g.n + 1 t.n = t.n * 10 return g.n end
        if both(g) ~= 20 then return -3 end
        local function hit(t) t.n = t.n - 1 end
        local a, b = hit(g)
        if a ~= nil or b ~= nil or select("#", hit(g)) ~= 0 or g.n ~= 18 then return -4 end
        return first + log
    "##,
    );
    assert_eq!(n, 5.0);
    // A typed loop does not call a leaf that stores: the store would change
    // what the loop reads.
    let n = eval_number(
        r#"
        acc = { v = 0 }
        function g(x) acc.v = acc.v + x return x end
        local s = 0
        for i = 1, 5 do s = g(i) end
        return s + acc.v
    "#,
    );
    assert_eq!(n, 20.0);
    let error = support::run_error(
        r#"
        local function add(t, n) t.x = t.x + n return t.x end
        add({ x = "s" }, 1)
        "#,
    );
    assert!(
        error
            .to_string()
            .contains("attempt to perform arithmetic on a string value"),
        "{error}"
    );
    let error = support::run_error(
        r#"
        local function add(t, n) t.x = t.x + n return t.x end
        add(7, 1)
        "#,
    );
    assert!(error.to_string().contains("number value"), "{error}");
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
