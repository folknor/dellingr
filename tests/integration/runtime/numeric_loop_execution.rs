//! Numeric `for` execution: ascending, stepped, descending and empty ranges,
//! and the typed loop region against canonical dispatch.
//!
//! The budget sweeps here are most of the integration crate's compute, so
//! `brokkr check` runs this module in each dev shape's parallel lane, beside
//! the standalone corpus tests, instead of the serial one (brokkr.toml, the
//! comment above the default sweep). Tests placed here therefore run
//! concurrently: each builds its own `State` and must keep to small memory.

/// This module's path; the lane partition in `corpus::run_examples` derives
/// its selector from it, so a rename fails there instead of moving the tests.
pub(crate) const MODULE: &str = module_path!();

use dellingr::{ArgCount, Charge, CostModel, Costing, RetCount, State};

use crate::support;
use crate::support::PushNewRustFn;
use crate::support::eval_number;

/// A loop whose body is straight-line arithmetic, a move and a literal load,
/// so it runs as a typed region, with a closure taken before the loop that
/// reads the captured locals after a failure. `{twin}` is spliced into the
/// body: a free upvalue load never read disqualifies the region without changing
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
    region_outcome_at(model, source, budget, None)
}

/// `region_outcome` with the host-set GC threshold `gc_threshold`: 1
/// collects at every back-edge checkpoint.
fn region_outcome_at(
    model: &CostModel,
    source: &str,
    budget: i64,
    gc_threshold: Option<usize>,
) -> (bool, u64, [f64; 3]) {
    let mut state = State::builder()
        .costing(Costing::Metered(model.clone()))
        .build();
    support::run(&mut state, source);
    if let Some(threshold) = gc_threshold {
        state.gc_set_threshold(threshold);
    }
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

/// A loop reading and writing captured numbers: a running sum that wraps at
/// a captured limit and a wrap count written on one arm only, an old value
/// of the sum read into a local before the write and used after it, and an
/// indexed read of `ys` that misses past 25 and answers through `__index`,
/// exiting the region mid-pass after this pass's writes. With `open` the
/// captures are locals of `run`, whose frame is live while `step` loops,
/// so the cells are open; otherwise they are closed before `run` is called.
fn upvalue_write_program(twin: &str, open: bool) -> String {
    let (outer, inner) = if open { ("", "local") } else { ("local", "") };
    format!(
        r#"
        local up = 0
        {outer} sum, wraps, limit = 0.5, 0, 97
        local ys = setmetatable({{}}, {{ __index = function(_, key) return key * 2 end }})
        for i = 1, 25 do ys[i] = 31 - i end
        function run()
            {inner} sum, wraps, limit = 0.5, 0, 97
            peek = function() return sum, wraps, limit end
            local function step()
                local d = 0
                for i = 1, 40 do
                    local old = sum
                    sum = sum + i + ys[i]
                    if sum > limit then
                        sum = sum - limit
                        wraps = wraps + 1
                    end
                    d = d + old
                    {twin}
                end
                return d
            end
            return step()
        end
        "#
    )
}

#[test]
fn numeric_loop_region_writing_upvalues_matches_canonical_dispatch_at_every_budget() {
    let models = [
        CostModel::default(),
        CostModel::default()
            .with(Charge::Add, 0)
            .with(Charge::Sub, 5),
        CostModel::zero(),
    ];
    for open in [false, true] {
        let region = upvalue_write_program("", open);
        let canonical = upvalue_write_program("local _u = up", open);
        for model in &models {
            for budget in -1..=900 {
                assert_eq!(
                    region_outcome(model, &region, budget),
                    region_outcome(model, &canonical, budget),
                    "budget {budget} under {model:?}, open {open}"
                );
            }
            // A collection at every back-edge checkpoint.
            assert_eq!(
                region_outcome_at(model, &region, i64::MAX, Some(1)),
                region_outcome_at(model, &canonical, i64::MAX, Some(1)),
                "collecting under {model:?}, open {open}"
            );
        }
    }
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
    // A typed loop calling a leaf that stores reads each field from its slot
    // on every call.
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

/// A typed loop making method calls to field leaves, one inherited two
/// `__index` levels up and kept for one result, one dropping its result,
/// and a plain call of a global field leaf for no result. `{twin}` is
/// spliced at the top of each callee: a free boolean load makes it no leaf,
/// which keeps the whole loop canonical without changing any charge.
fn method_region_program(twin: &str) -> String {
    format!(
        r#"
        local stats = {{ calls = 0 }}
        local Base = {{}}
        Base.__index = Base
        function Base:inc(n)
            {twin}
            stats.calls = stats.calls + 1
            self.count = self.count + n * 2
            return self.count
        end
        local Sub = setmetatable({{}}, {{ __index = Base }})
        Sub.__index = Sub
        function Sub:damage(n)
            {twin}
            self.hp = self.hp - n
        end
        function bump(x)
            {twin}
            stats.calls = stats.calls + x
        end
        local c = setmetatable({{ count = 0.5, hp = 100 }}, Sub)
        peek = function() return c.count + stats.calls, c.hp, stats.calls end
        function run()
            local sum, k = 0, 0
            for i = 1, 30 do
                sum = c:inc(i)
                c:damage(i * 0.5)
                bump(1)
                k = k + c:inc(1) - sum
            end
            return sum + k
        end
        "#
    )
}

#[test]
fn numeric_loop_region_with_method_calls_matches_canonical_dispatch_at_every_budget() {
    let region = method_region_program("");
    let canonical = method_region_program("local _u = false");
    let models = [
        CostModel::default(),
        CostModel::default()
            .with(Charge::TableWrite, 4)
            .with(Charge::Mul, 0)
            .with(Charge::Sub, 5),
        CostModel::zero(),
    ];
    for model in &models {
        for budget in -1..=1200 {
            assert_eq!(
                region_outcome(model, &region, budget),
                region_outcome(model, &canonical, budget),
                "budget {budget} under {model:?}"
            );
        }
    }
}

#[test]
fn numeric_loop_region_method_calls_decline_what_a_store_could_change() {
    // `hit` stores a number under the key the next call looks its method up
    // by, so the region declines and the second call fails as a call does.
    let error = support::run_error(
        r#"
        local M = {}
        M.__index = M
        function M:hit() self.f = 1 end
        local o = setmetatable({}, M)
        o.f = function(self) end
        for i = 1, 3 do
            o:hit()
            o:f()
        end
        "#,
    );
    assert!(error.to_string().contains("attempt to call"), "{error}");
    // A method found through a function `__index`, a method that is no
    // leaf, and a receiver that is not a table run canonically.
    let n = eval_number(
        r#"
        local calls = 0
        local P = setmetatable({}, { __index = function(t, k)
            calls = calls + 1
            return function(self, x) return x + 1 end
        end })
        local s = 0
        for i = 1, 4 do s = P:m(s) end
        local Q = { v = 0 }
        function Q:m(x) if x then self.v = self.v + x end return self.v end
        for i = 1, 4 do s = s + Q:m(i) end
        return s * 100 + calls
    "#,
    );
    assert_eq!(n, 2404.0);
    // A method capturing a local of the chunk that is still running: the
    // capture is open on the chunk's frame, below the looping function's.
    let n = eval_number(
        r#"
        local stats = { calls = 0 }
        local C = {}
        C.__index = C
        function C:inc(n) stats.calls = stats.calls + 1 self.v = self.v + n end
        local o = setmetatable({ v = 0 }, C)
        local function loop() for i = 1, 10 do o:inc(i) end end
        loop()
        loop()
        return o.v * 100 + stats.calls
    "#,
    );
    assert_eq!(n, 11020.0);
    let error = support::run_error(
        r#"
        local o = 5
        for i = 1, 3 do o:m(i) end
        "#,
    );
    assert!(error.to_string().contains("attempt to index"), "{error}");
}

/// Method calls on records read per pass: three classes behind one site,
/// one inheriting two `__index` levels up, one record holding the method
/// itself, one class whose method is no leaf, one record inheriting a field
/// its method reads, and a method storing into its record, which the loop
/// then reads and stores. The loop is nested in a round loop, with a branch
/// beside the call and a `break` out of the inner loop. `{twin}` goes into
/// the inner body, where an unread upvalue load keeps every loop canonical,
/// and `{leaf_twin}` into every leaf, where a free boolean load makes it no
/// leaf; neither changes a charge.
fn record_method_program(twin: &str, leaf_twin: &str) -> String {
    format!(
        r#"
        local up = 0
        local stats = {{ calls = 0 }}
        local Shape = {{}}
        Shape.__index = Shape
        function Shape:area()
            {leaf_twin}
            return self.w * self.h
        end
        local Rect = setmetatable({{}}, {{ __index = Shape }})
        Rect.__index = Rect
        Rect.h = 5
        local Circle = {{}}
        Circle.__index = Circle
        function Circle:area()
            {leaf_twin}
            stats.calls = stats.calls + 1
            self.r = self.r + 0.5
            return 3 * self.r
        end
        local Odd = {{}}
        Odd.__index = Odd
        function Odd:area() if self.w then return 1 end return 2 end
        local shapes = {{}}
        function run()
            local sum, k = 0.5, 0
            peek = function()
                local c = 0
                for i = 1, 9 do
                    local s = shapes[i]
                    if s then c = c + (rawget(s, "r") or 0) * i end
                end
                return sum, c + k, stats.calls
            end
            for i = 1, 9 do
                if i % 3 == 0 then
                    shapes[i] = setmetatable({{ w = i, h = 2 }}, Rect)
                else
                    shapes[i] = setmetatable({{ r = i }}, Circle)
                end
            end
            shapes[5] = setmetatable({{ w = 1, h = 1 }}, Shape)
            shapes[8] = setmetatable({{ w = 1 }}, Rect)
            shapes[9] = setmetatable({{ w = 4, area = function(self) return 7 end }}, Circle)
            shapes[7] = setmetatable({{ w = 1 }}, Odd)
            for round = 1, 3 do
                for i = 1, 9 do
                    if round == 3 and i == 6 then break end
                    local s = shapes[i]
                    sum = sum + s:area()
                    if sum > 40 then sum = sum - 40 end
                    k = k + i
                    {twin}
                end
                k = k + round
            end
            return sum
        end
        "#
    )
}

#[test]
fn numeric_loop_region_with_method_calls_on_records_matches_canonical_dispatch_at_every_budget() {
    let region = record_method_program("", "");
    let canonical = record_method_program("local _u = up", "local _u = false");
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
                region_outcome(model, &region, budget),
                region_outcome(model, &canonical, budget),
                "budget {budget} under {model:?}"
            );
        }
    }
}

/// Field leaves with forward branches: a guard on a field's truthiness (a
/// boolean, a number the leaf itself stores into the tested field first, a
/// table), compare-branches with an `else`, called from canonical dispatch
/// and from a region over records, with charges that differ per path.
/// `{leaf_twin}` keeps every leaf canonical and `{twin}` the region.
fn branching_leaf_program(twin: &str, leaf_twin: &str) -> String {
    format!(
        r#"
        local up = 0
        local Body = {{}}
        Body.__index = Body
        function Body:step(dt)
            {leaf_twin}
            if self.alive then
                self.x = self.x + self.vx * dt
                if self.x > 30 then self.x = self.x - 25 else self.age = self.age + 1 end
            end
        end
        function Body:flip(n)
            {leaf_twin}
            self.mark = n
            if not self.mark then self.age = self.age * 2 end
            if self.tag then self.age = self.age + n / 4 end
            return self.age
        end
        local bodies = {{}}
        for i = 1, 8 do
            bodies[i] = setmetatable({{ alive = i % 3 ~= 0, x = i, vx = i % 4 - 1, age = 0, mark = false }}, Body)
        end
        bodies[5].tag = bodies
        bodies[7].alive = nil
        function run()
            local sum, k = 0.5, 0
            peek = function()
                local c = 0
                for i = 1, 8 do c = c + bodies[i].x * i + bodies[i].age end
                return sum, c, k
            end
            for round = 1, 4 do
                for i = 1, 8 do
                    bodies[i]:step(2)
                    {twin}
                end
                for i = 1, 8 do
                    sum = sum + bodies[i]:flip(round)
                end
                k = k + round
            end
            return sum
        end
        "#
    )
}

#[test]
fn branching_field_leaves_match_canonical_dispatch_at_every_budget() {
    let fast = branching_leaf_program("", "");
    let canonical = branching_leaf_program("local _u = up", "local _u = false");
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
                region_outcome(model, &fast, budget),
                region_outcome(model, &canonical, budget),
                "budget {budget} under {model:?}"
            );
        }
    }
}

/// A straight-line loop body calling leaves with branches: a global leaf
/// on numbers and a field leaf of a loop-invariant receiver. The body has
/// no branch of its own, so the straight-line runner sees the call first
/// and hands the loop to the control runner. `{twin}` keeps the loop
/// canonical, `{leaf_twin}` every leaf.
fn straight_branching_leaf_program(twin: &str, leaf_twin: &str) -> String {
    format!(
        r#"
        local up = 0
        local Body = {{}}
        Body.__index = Body
        function clamp(x, hi)
            {leaf_twin}
            if x > hi then x = x - hi * 0.75 else x = x + 1.5 end
            return x
        end
        function Body:pull(d)
            {leaf_twin}
            if self.alive then self.x = self.x + d end
            if self.x > 20 then self.x = self.x / 3 end
            return self.x
        end
        local b = setmetatable({{ alive = true, x = 1 }}, Body)
        function run()
            local sum, last, k = 0.5, 0, 0
            peek = function() return sum, last, k + b.x end
            for i = 1, 40 do
                sum = clamp(sum + i, 30)
                last = b:pull(i % 5)
                k = k + last * 0.5
                {twin}
            end
            return sum
        end
        "#
    )
}

#[test]
fn straight_line_loops_calling_branching_leaves_match_canonical_dispatch_at_every_budget() {
    let fast = straight_branching_leaf_program("", "");
    let canonical = straight_branching_leaf_program("local _u = up", "local _u = false");
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
                region_outcome(model, &fast, budget),
                region_outcome(model, &canonical, budget),
                "budget {budget} under {model:?}"
            );
            assert_eq!(
                region_outcome_at(model, &fast, budget, Some(1)),
                region_outcome_at(model, &canonical, budget, Some(1)),
                "budget {budget} under {model:?}, collecting at every checkpoint"
            );
        }
    }
}

/// Nested loops inside one region: an outer pass reading two upvalue
/// tables, a number upvalue as the inner limit, an inner loop that may not
/// enter, a forward branch out of the inner loop, and a third loop nested
/// in the second. `{twin}` in the innermost body keeps every loop canonical.
fn nested_region_program(twin: &str) -> String {
    format!(
        r#"
        local up = 0
        local n = 6
        local xs, ys = {{}}, {{}}
        for i = 1, 8 do xs[i] = i * 3 % 7 ys[i] = i * 5 % 9 end
        function run()
            local sum, hits, last = 0.5, 0, 0
            peek = function() return sum, hits, last end
            for i = 1, 8 do
                local a = xs[i]
                for j = i, n do
                    if ys[j] > a + 4 then break end
                    for k = 1, j % 3 do
                        sum = sum + k * a - j
                        {twin}
                    end
                    hits = hits + 1
                end
                last = i + a
            end
            return sum
        end
        "#
    )
}

#[test]
fn numeric_loop_region_with_nested_loops_matches_canonical_dispatch_at_every_budget() {
    let region = nested_region_program("");
    let canonical = nested_region_program("local _u = up");
    let models = [
        CostModel::default(),
        CostModel::default()
            .with(Charge::Mul, 0)
            .with(Charge::Sub, 5),
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

/// A loop reading a record from a local table on every iteration, then
/// reading, updating and storing its fields. Record 5 has no raw `x`, so its
/// first read answers through `__index` and its store makes a new key
/// through `__newindex`; record 3 has no `hp`, so its first store exits
/// before its charge; record 8 is the outer table itself. The inner loop is
/// entered once per round, so it runs as a region again after each exit.
fn record_region_program(twin: &str) -> String {
    format!(
        r#"
        local up = 0
        local log = 0
        local M = {{
            __index = function(t, k) return 0.5 end,
            __newindex = function(t, k, v) log = log + 1 rawset(t, k, v) end,
        }}
        function run()
            local sum, k = 0.5, 0
            local us
            peek = function()
                local c = 0
                for i = 1, 12 do
                    local u = us and us[i]
                    if u then c = c + (rawget(u, "x") or 0) * i + (rawget(u, "hp") or 0) end
                end
                return sum, c, log
            end
            us = {{}}
            for i = 1, 12 do
                us[i] = setmetatable({{ x = i, speed = i % 3, hp = 0, a = 1, b = 2 }}, M)
            end
            us[5] = setmetatable({{ speed = 2, hp = 0 }}, M)
            us[3] = setmetatable({{ x = 3, speed = 1 }}, M)
            us[8] = us
            us.x, us.speed, us.hp = 1, 4, 0
            for round = 1, 3 do
                for i = 1, 12 do
                    local u = us[i]
                    u.x = u.x + u.speed
                    sum = sum + u.x
                    u.hp = i
                    k = i
                    {twin}
                end
            end
            return sum
        end
        "#
    )
}

#[test]
fn numeric_loop_region_with_records_matches_canonical_dispatch_at_every_budget() {
    let region = record_region_program("");
    let canonical = record_region_program("local _u = up");
    let models = [
        CostModel::default(),
        CostModel::default()
            .with(Charge::TableWrite, 4)
            .with(Charge::Add, 0),
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

/// `ipairs` loops whose value is a record, bound afresh every pass. Key 1
/// is a raw miss answered by `__index` with a fresh receiver of a fresh
/// class, which dies once the step replaces it; record 6 inherits `x`, so
/// its first store exits before its charge; record 7 is the iterated table
/// itself; from the second round key 5 holds `true`, which the body skips
/// but the bind refuses, handing the rest of that loop back. Run with a
/// collection at every checkpoint too.
fn ipairs_record_program(twin: &str) -> String {
    format!(
        r#"
        local up = 0
        local stats = {{ calls = 0 }}
        local Unit = {{ x = 0.5 }}
        Unit.__index = Unit
        function Unit:speed() return self.vx * 2 end
        local Ghost = {{}}
        Ghost.__index = Ghost
        function Ghost:speed() stats.calls = stats.calls + 1 return 1 end
        local Src = {{}}
        Src.__index = function(t, key)
            if key == 1 then
                local class = {{}}
                class.__index = {{ speed = function(self) return self.vx + 0.5 end }}
                return setmetatable({{ x = 100, vx = 3 }}, class)
            end
        end
        function run()
            local sum, k = 0.5, 0
            local us
            peek = function()
                local c = 0
                for i = 2, 10 do
                    local u = us and rawget(us, i)
                    if u and u ~= true then c = c + (rawget(u, "x") or 0) * i end
                end
                return sum, c + k, stats.calls
            end
            us = setmetatable({{}}, Src)
            for i = 2, 10 do
                us[i] = setmetatable({{ x = i, vx = i % 3 }}, i % 4 == 0 and Ghost or Unit)
            end
            us[6] = setmetatable({{ vx = 1 }}, Unit)
            us.x, us.vx = 1, 2
            for round = 1, 3 do
                if round == 2 then us[5] = true end
                us[7] = nil
                for i, u in ipairs(us) do
                    if i ~= 5 then
                        u.x = u.x + u.vx
                        sum = sum + u:speed()
                    end
                    k = k + i
                    {twin}
                end
                us[7] = us
                for i, u in ipairs(us) do
                    if i ~= 5 then
                        u.vx = u.vx + 1
                        sum = sum + u.x * 0.5
                    end
                    {twin}
                end
            end
            return sum
        end
        "#
    )
}

#[test]
fn ipairs_region_with_records_matches_canonical_dispatch_at_every_budget() {
    let region = ipairs_record_program("");
    let canonical = ipairs_record_program("local _u = up");
    let models = [
        CostModel::default(),
        CostModel::default()
            .with(Charge::TableWrite, 4)
            .with(Charge::Add, 0),
        CostModel::zero(),
    ];
    for model in &models {
        for gc_threshold in [None, Some(1)] {
            for budget in -1..=900 {
                assert_eq!(
                    region_outcome_at(model, &region, budget, gc_threshold),
                    region_outcome_at(model, &canonical, budget, gc_threshold),
                    "budget {budget} under {model:?}, threshold {gc_threshold:?}"
                );
            }
        }
    }
}

/// Records read by a string key held in a register (`us[i][key]`), the key
/// moving between entries: misses through `__index`, a key stored through
/// a literal of the same name in the same body, records of other layouts,
/// a record that is its own source table, a record replaced between
/// entries, and a number key, which declines the region at entry.
fn keyed_record_program(twin: &str) -> String {
    format!(
        r#"
        local up = 0
        local M = {{ __index = function(t, k) return 0.25 end }}
        function run()
            local sum, rounds = 0.5, 0
            local us
            peek = function()
                local c = 0
                for i = 1, 12 do
                    local u = us and us[i]
                    if u then c = c + (rawget(u, "x") or 0) * i end
                end
                return sum, c, rounds
            end
            us = {{}}
            for i = 1, 12 do
                us[i] = setmetatable({{ hp = i, armor = i % 4, speed = 2, x = 1 }}, M)
            end
            us[4] = setmetatable({{ speed = 3, hp = 1, armor = 0, x = 2 }}, M)
            us[6] = setmetatable({{ armor = 9 }}, M)
            us[9] = setmetatable({{ hp = 1, speed = 1, armor = 1, x = 0 }}, M)
            us[11] = setmetatable(us, M)
            us.hp, us.armor, us.speed, us.x = 1, 2, 3, 4
            local keys = {{ "hp", "armor", "speed", "x", 13, "missing", "hp" }}
            for round = 1, #keys do
                local key = keys[round]
                rounds = round
                if round == 7 then us[6] = setmetatable({{ hp = 2 }}, M) end
                for i = 1, 12 do
                    local u = us[i]
                    u.x = u[key] + u.x
                    sum = sum + u[key]
                    {twin}
                end
            end
            return sum
        end
        "#
    )
}

#[test]
fn numeric_loop_region_with_keyed_records_matches_canonical_dispatch_at_every_budget() {
    let region = keyed_record_program("");
    let canonical = keyed_record_program("local _u = up");
    let models = [
        CostModel::default(),
        CostModel::default()
            .with(Charge::TableWrite, 4)
            .with(Charge::Add, 0),
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

/// A loop truth-testing record fields, `if u.alive then` and
/// `if not u.dead then`, the tested register reused for a number on the
/// taken path. `alive` holds booleans, numbers (zero is true), a table that
/// the register then roots, and on record 7 nothing, a raw miss answered by
/// `__index`, which exits the region at its lookup; the inner loop is
/// entered once per round, so it runs as a region again after each exit.
/// Run with a collection at every checkpoint too.
fn record_test_program(twin: &str) -> String {
    format!(
        r#"
        local up = 0
        local M = {{ __index = function(t, k) return k == "alive" end }}
        function run()
            local sum, k = 0.5, 0
            local us
            peek = function()
                local c = 0
                for i = 1, 12 do
                    local u = us and us[i]
                    if u then c = c + (rawget(u, "x") or 0) * i end
                end
                return sum, c, k
            end
            us = {{}}
            for i = 1, 12 do
                local alive = i % 3 ~= 0
                if i == 4 then alive = 0 elseif i == 5 then alive = {{}} end
                us[i] = setmetatable({{ x = i, speed = i % 4, alive = alive, dead = i == 2 }}, M)
            end
            us[7] = setmetatable({{ x = 7, speed = 1, dead = false }}, M)
            for round = 1, 3 do
                for i = 1, 12 do
                    local u = us[i]
                    if u.alive then
                        u.x = u.x + u.speed
                    end
                    if not u.dead then
                        sum = sum + u.x
                    end
                    k = i
                    {twin}
                end
            end
            return sum
        end
        "#
    )
}

#[test]
fn numeric_loop_region_with_record_tests_matches_canonical_dispatch_at_every_budget() {
    let region = record_test_program("");
    let canonical = record_test_program("local _u = up");
    let models = [
        CostModel::default(),
        CostModel::default()
            .with(Charge::TableWrite, 4)
            .with(Charge::Add, 0),
        CostModel::zero(),
    ];
    for model in &models {
        for gc_threshold in [None, Some(1)] {
            for budget in -1..=700 {
                assert_eq!(
                    region_outcome_at(model, &region, budget, gc_threshold),
                    region_outcome_at(model, &canonical, budget, gc_threshold),
                    "budget {budget} under {model:?}, threshold {gc_threshold:?}"
                );
            }
        }
    }
}

/// Truth tests whose value is a fresh table that a number store then
/// replaces in its field, so the tested local `a` is its only root: after
/// the first loop it still holds the last one, which `census` (a full
/// collection, then the live object count) sees; in the second a number
/// read into `a` drops it. The tables are refilled every round. `{twin}`
/// must leave the register layout alone, since dead registers are roots
/// too.
fn record_test_root_program(twin: &str) -> String {
    format!(
        r#"
        local up = {{}}
        keep = up
        local function refill(us)
            for i = 1, 12 do us[i].alive = {{}} end
        end
        function run()
            local us = {{}}
            for i = 1, 12 do us[i] = {{ x = i }} end
            local sum, k, seen = 0.5, 0, 0
            local a
            for round = 1, 3 do
                refill(us)
                for i = 1, 12 do
                    local u = us[i]
                    a = u.alive
                    if a then u.alive = i end
                    {twin}
                    k = i
                end
                seen = seen + census()
                refill(us)
                for i = 1, 12 do
                    local u = us[i]
                    a = u.alive
                    if a then u.alive = i end
                    a = u.x
                    sum = sum + a
                    {twin}
                    k = i
                end
                seen = seen + census()
            end
            return sum + seen
        end
        "#
    )
}

/// `run()` of `source` under `budget` with a collection at every
/// checkpoint: whether it completed, the meter, and the live object count
/// right after it returned, which the roots at its last checkpoint decided.
fn rooted_outcome(model: &CostModel, source: &str, budget: i64) -> (Option<f64>, u64, usize) {
    let mut state = State::builder()
        .costing(Costing::Metered(model.clone()))
        .build();
    state
        .push_new_rust_fn(|state| {
            state.gc_collect();
            let live = state.object_count();
            state.push_number(live as f64)?;
            Ok(1)
        })
        .expect("census is pushed");
    state.set_global("census").expect("census is installed");
    support::run(&mut state, source);
    state.gc_set_threshold(1);
    state.set_cost_budget(budget).expect("the State is metered");
    state.get_global("run").expect("run is defined");
    let result = match state.call(ArgCount::Fixed(0), RetCount::Fixed(1)) {
        Ok(_) => Some(state.to_number(-1).expect("run returns a number")),
        Err(error) => {
            assert!(
                matches!(
                    error.kind(),
                    dellingr::error::ErrorKind::BudgetExceeded { .. }
                ),
                "budget {budget} returned {error:?}"
            );
            None
        }
    };
    (result, state.cost_used(), state.object_count())
}

#[test]
fn record_tests_root_what_canonical_dispatch_roots_at_every_budget() {
    let region = record_test_root_program("");
    // A free load of a table upvalue into `k`, overwritten next, refuses
    // the region without moving any register, so every dead register holds
    // what it would in the region.
    let canonical = record_test_root_program("k = up");
    let models = [CostModel::default(), CostModel::zero()];
    for model in &models {
        for budget in -1..=600 {
            assert_eq!(
                rooted_outcome(model, &region, budget),
                rooted_outcome(model, &canonical, budget),
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
