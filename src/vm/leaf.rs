//! Library leaves: standard-library functions the dispatch loop executes
//! directly when Lua calls them, instead of entering them as Rust callbacks.
//!
//! A leaf is a catalog entry (`lua_std::leaf_of`) whose work is a pure
//! computation on its arguments: it runs no host code and no Lua, so it needs
//! none of the callback ABI `invoke_rust_fn` provides (callee-slot removal,
//! frame-bottom and active-function bookkeeping, panic containment, the
//! floating-point environment check at return, result relocation). The
//! callee is recognised by its evaluated value, as generic `for` recognises
//! `next`, so aliases (`local floor = math.floor`), rebinding and replaced
//! library tables need no compile-time reasoning, and a host entry is never a
//! leaf whatever function it calls.
//!
//! Execution is semantically transparent: results, costs, errors and their
//! order, stack-cap refusal points, fault-injection events, collection timing
//! and the random stream are those of the callback. To keep that true:
//!
//! - Anything off a leaf's success path (a missing or ill-typed argument, an
//!   empty `math.random` interval, a `tonumber` base) declines before any side
//!   effect, and the callback runs and raises its own error, after its own
//!   charge.
//! - A committed leaf takes the callback's steps in the callback's order: the
//!   entry interrupt poll `invoke_rust_fn` takes, then the flat charge (as
//!   `State::charge`, whose poison and environment checks cannot fail while
//!   dispatch runs library code), then the computation, then a stack-slot
//!   check at each length the callback pushed at (`check_stack_slot_at`),
//!   then the writes. Its results land where a callback's do, from the
//!   callee's slot, and never need more slots than the call window held.
//! - The computations the callbacks and leaves share live once, in the
//!   library (`lua_std::math`).
//!
//! Under the debug-only `disable-inline-caches` feature every leaf declines
//! while cache hits are off, so the inline-cache differential compares leaf
//! execution with the callback path for the whole example corpus.

use super::{Result, State, Val};
use crate::cost_model::Charge;
use crate::lua_std::math;
use crate::numeral::{exact_i64, parse_lua_numeral};

/// A standard-library function the dispatch loop executes directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Leaf {
    Floor,
    Ceil,
    Sqrt,
    Abs,
    Deg,
    Rad,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Exp,
    Atan,
    Log,
    Atan2,
    Fmod,
    Min,
    Max,
    Modf,
    Random,
    Type,
    RawEqual,
    RawLen,
    ToNumber,
    RawGet,
}

impl State {
    /// If the callee at `idx` (with `num_args` arguments above it and
    /// nothing beyond) is a library leaf, execute it as its callback would.
    /// Returns the result count, with the results from `idx` and the stack
    /// ending after them, or `None`, having changed nothing, when the callee
    /// is not a leaf or its callback must run instead.
    ///
    /// Out of line, catalog recognition included: the callers' frames stay
    /// live across native re-entry when the callee is a callback, so none of
    /// this belongs in them. The callers test only that the callee is a Rust
    /// function first, so a Lua callee pays no call here.
    #[inline(never)]
    pub(super) fn call_leaf(&mut self, idx: usize, num_args: u8) -> Result<Option<u8>> {
        let Val::RustFn(function) = self.stack[idx] else {
            return Ok(None);
        };
        let Some(leaf) = crate::lua_std::leaf_of(function) else {
            return Ok(None);
        };
        #[cfg(feature = "disable-inline-caches")]
        if super::caches::hits_disabled() {
            return Ok(None);
        }
        #[cfg(test)]
        if callback_forced() {
            return Ok(None);
        }
        let n = usize::from(num_args);
        debug_assert_eq!(self.stack.len(), idx + 1 + n);
        let arg = |state: &Self, i: usize| (i < n).then(|| state.stack[idx + 1 + i]);
        let num = |state: &Self, i: usize| match arg(state, i) {
            Some(Val::Num(x)) => Some(x),
            _ => None,
        };
        // An optional numeric argument: absent or nil is `Some(None)`, a
        // number `Some(Some(x))`, anything else `None` (the callback errors).
        let opt_num = |state: &Self, i: usize| match arg(state, i) {
            None | Some(Val::Nil) => Some(None),
            Some(Val::Num(x)) => Some(Some(x)),
            Some(_) => None,
        };
        let unary =
            |state: &Self, charge: Charge, f: fn(f64) -> f64| num(state, 0).map(|x| (charge, f(x)));
        let number = match leaf {
            Leaf::Floor => unary(self, Charge::MathFloor, f64::floor),
            Leaf::Ceil => unary(self, Charge::MathCeil, f64::ceil),
            Leaf::Sqrt => unary(self, Charge::MathSqrt, f64::sqrt),
            Leaf::Abs => unary(self, Charge::MathAbs, f64::abs),
            Leaf::Deg => unary(self, Charge::MathDeg, f64::to_degrees),
            Leaf::Rad => unary(self, Charge::MathRad, f64::to_radians),
            Leaf::Sin => unary(self, Charge::MathSin, crate::crmath::sin),
            Leaf::Cos => unary(self, Charge::MathCos, crate::crmath::cos),
            Leaf::Tan => unary(self, Charge::MathTan, crate::crmath::tan),
            Leaf::Asin => unary(self, Charge::MathAsin, crate::crmath::asin),
            Leaf::Acos => unary(self, Charge::MathAcos, crate::crmath::acos),
            Leaf::Exp => unary(self, Charge::MathExp, crate::crmath::exp),
            Leaf::Atan => num(self, 0)
                .zip(opt_num(self, 1))
                .map(|(y, x)| (Charge::MathAtan, math::atan_of(y, x))),
            Leaf::Log => num(self, 0)
                .zip(opt_num(self, 1))
                .map(|(x, base)| (Charge::MathLog, math::log_of(x, base))),
            Leaf::Atan2 => num(self, 0)
                .zip(num(self, 1))
                .map(|(y, x)| (Charge::MathAtan2, crate::crmath::atan2(y, x))),
            Leaf::Fmod => num(self, 0)
                .zip(num(self, 1))
                .map(|(x, y)| (Charge::MathFmod, x % y)),
            Leaf::Min | Leaf::Max => {
                let (charge, step): (_, fn(f64, f64) -> f64) = if leaf == Leaf::Min {
                    (Charge::MathMin, math::min_step)
                } else {
                    (Charge::MathMax, math::max_step)
                };
                let mut result = num(self, 0);
                // bounded: one step per argument, at most MAX_ARGUMENTS.
                for i in 1..n {
                    result = result.zip(num(self, i)).map(|(r, v)| step(r, v));
                }
                result.map(|r| (charge, r))
            }
            Leaf::Modf => return self.leaf_modf(idx, num(self, 0)),
            Leaf::Random => return self.leaf_random(idx, n),
            Leaf::Type => return self.leaf_type(idx, n),
            Leaf::RawEqual => {
                if n < 2 {
                    return Ok(None);
                }
                let equal = self.stack[idx + 1] == self.stack[idx + 2];
                return self.leaf_free_result(idx, Val::Bool(equal));
            }
            Leaf::RawLen => {
                let Some(value) = arg(self, 0) else {
                    return Ok(None);
                };
                let len = if let Some(bytes) = value.as_string(&self.heap) {
                    bytes.len()
                } else if let Some(table) = value
                    .as_object_ptr()
                    .and_then(|ptr| self.heap.as_table_ref(ptr))
                {
                    table.array_len()
                } else {
                    return Ok(None);
                };
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "rawlen reports a length as a Lua number, as its callback does."
                )]
                let len = len as f64;
                return self.leaf_free_result(idx, Val::Num(len));
            }
            Leaf::ToNumber => {
                let Some(value) = arg(self, 0) else {
                    return Ok(None);
                };
                if !matches!(arg(self, 1), None | Some(Val::Nil)) {
                    return Ok(None);
                }
                let result = match value {
                    Val::Num(_) => value,
                    _ => match value.as_string(&self.heap) {
                        Some(bytes) => parse_lua_numeral(bytes).map_or(Val::Nil, Val::Num),
                        None => Val::Nil,
                    },
                };
                return self.leaf_free_result(idx, result);
            }
            Leaf::RawGet => {
                if n < 2 {
                    return Ok(None);
                }
                let Some(table) = self.stack[idx + 1]
                    .as_object_ptr()
                    .and_then(|ptr| self.heap.as_table_ref(ptr))
                else {
                    return Ok(None);
                };
                let value = table.get(&self.stack[idx + 2]);
                self.enter_leaf(None)?;
                // The callback cuts its arguments to two and pushes the key.
                self.check_stack_slot_at(idx + 2)?;
                self.stack.truncate(idx);
                self.push_unchecked(value);
                return Ok(Some(1));
            }
        };
        let Some((charge, result)) = number else {
            return Ok(None);
        };
        self.enter_leaf(Some(charge))?;
        self.leaf_result(idx, Val::Num(result))?;
        Ok(Some(1))
    }

    /// A committed leaf's entry: the callback entry's interrupt poll, then
    /// the leaf's flat charge, if it has one.
    #[inline(always)]
    fn enter_leaf(&mut self, charge: Option<Charge>) -> Result<()> {
        self.poll_interrupt()?;
        if let Some(charge) = charge {
            self.charge_cost(self.cost.cost(charge, 1))?;
        }
        Ok(())
    }

    /// Replace the call window with one result, checking the slot the
    /// callback's push checked.
    #[inline(always)]
    fn leaf_result(&mut self, idx: usize, value: Val) -> Result<()> {
        self.stack.truncate(idx);
        self.check_stack_slot_at(idx)?;
        self.push_unchecked(value);
        Ok(())
    }

    /// Commit a free leaf with one already computed result.
    fn leaf_free_result(&mut self, idx: usize, value: Val) -> Result<Option<u8>> {
        self.enter_leaf(None)?;
        self.leaf_result(idx, value)?;
        Ok(Some(1))
    }

    fn leaf_modf(&mut self, idx: usize, x: Option<f64>) -> Result<Option<u8>> {
        let Some(x) = x else {
            return Ok(None);
        };
        self.enter_leaf(Some(Charge::MathModf))?;
        let (integral, fractional) = math::modf_of(x);
        self.leaf_result(idx, Val::Num(integral))?;
        self.check_stack_slot_at(idx + 1)?;
        self.push_unchecked(Val::Num(fractional));
        Ok(Some(2))
    }

    /// `math.random`: the arguments are validated before the commit, and the
    /// draw is made before the result's slot check, as the callback does, so
    /// a refused push leaves the stream advanced exactly as there.
    fn leaf_random(&mut self, idx: usize, n: usize) -> Result<Option<u8>> {
        let integer = |state: &Self, i: usize| match state.stack[idx + 1 + i] {
            Val::Num(x) => exact_i64(x),
            _ => None,
        };
        let range = match n {
            0 => None,
            1 => match integer(self, 0) {
                Some(high) if high >= 1 => Some((1, high)),
                _ => return Ok(None),
            },
            2 => match (integer(self, 0), integer(self, 1)) {
                (Some(low), Some(high)) if low <= high => Some((low, high)),
                _ => return Ok(None),
            },
            _ => return Ok(None),
        };
        self.enter_leaf(Some(Charge::MathRandom))?;
        let result = match range {
            None => self.rng.random_f64(),
            Some((low, high)) => math::draw_range(self, low, high)?,
        };
        self.leaf_result(idx, Val::Num(result))?;
        Ok(Some(1))
    }

    /// `type`: the name is interned after the window is cut, as the callback
    /// interns it after clearing its frame, so a collection the allocation
    /// triggers sees the same roots.
    fn leaf_type(&mut self, idx: usize, n: usize) -> Result<Option<u8>> {
        if n == 0 {
            return Ok(None);
        }
        let name = self.stack[idx + 1].typ(&self.heap).as_str();
        self.enter_leaf(None)?;
        self.stack.truncate(idx);
        self.check_stack_slot_at(idx)?;
        let value = self.alloc_string(name)?;
        self.push_unchecked(value);
        Ok(Some(1))
    }
}

#[cfg(test)]
std::thread_local! {
    /// Test switch: every leaf declines, so the callback runs instead.
    static FORCE_CALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn callback_forced() -> bool {
    FORCE_CALLBACK.get()
}

#[cfg(test)]
mod tests {
    use super::FORCE_CALLBACK;
    use crate::State;
    use crate::instr::{ArgCount, RetCount};

    /// Lua that calls each leaf on its success path and off it: wrong and
    /// missing arguments, extra arguments, method calls, tail calls, open
    /// and fixed result counts, aliases and a rebound library table.
    const CASES: &[&str] = &[
        "return math.floor(2.5), math.ceil(-2.5), math.sqrt(2), math.abs(-0.0)",
        "return math.deg(1), math.rad(90), math.sin(1), math.cos(1), math.tan(1)",
        "return math.asin(0.5), math.acos(0.5), math.exp(1)",
        "return math.atan(1), math.atan(1, 2), math.atan(1, nil), math.log(8), math.log(8, 2), math.log(1000, 10), math.log(8, nil)",
        "return math.atan2(1, -1), math.fmod(-5, 3), math.fmod(5, -0.0)",
        "return math.min(3, 1, 2), math.max(3, 1, 2), math.min(0.0, -0.0), math.max(0/0, 1), math.min(1, 0/0)",
        "return math.modf(3.7), math.modf(-1/0), math.modf(-0.0)",
        "local a, b = math.modf(2.5) return a, b",
        "local a = math.modf(2.5) return a",
        "return math.random(), math.random(10), math.random(-3, 3), math.random(5, 5), math.random(2^53, 2^53 + 4)",
        "return type(nil), type(1), type('x'), type({}), type(type), type(print, 1)",
        "local t = {} return rawequal(t, t), rawequal(t, {}), rawequal(1, 1.0), rawequal(nil, nil), rawequal('a', 'a')",
        "return rawlen({1, 2, 3}), rawlen('abcd'), rawlen(setmetatable({1}, {__len = function() return 9 end}))",
        "return tonumber(5), tonumber('0x10'), tonumber(' 12 '), tonumber('z'), tonumber({}), tonumber(nil), tonumber('7', nil)",
        "local t = setmetatable({a = 1}, {__index = function() return 2 end}) return rawget(t, 'a'), rawget(t, 'b'), rawget(t, 'a', 'extra')",
        "local t = {} return rawget(t, t), rawget({[1] = t}, 1) == t",
        "local floor = math.floor local function f(x) return floor(x) end return f(1.5), f(-1.5)",
        "local function f(x) return type(x) end return f(1), f('s')",
        "local m = math math = {floor = function() return 'mine' end} local r = math.floor(1) math = m return r, math.floor(1.5)",
        "local o = {floor = math.floor} return o:floor()",
        "return math.floor()",
        "return math.floor('1')",
        "return math.min()",
        "return math.max(1, 'x')",
        "return math.random(0)",
        "return math.random(3, 1)",
        "return math.random(1.5)",
        "return math.random(1, 2, 3)",
        "return math.atan(1, 'x')",
        "return type()",
        "return rawequal(1)",
        "return rawlen(1)",
        "return tonumber()",
        "return tonumber('10', 2), tonumber('z', 36)",
        "return tonumber(10, 2)",
        "return rawget(1, 1)",
        "return rawget({})",
        "local o = {} o.t = type return o:t(), select('#', o:t())",
    ];

    const SHOW: &str = "local function show(...) \
        local s = tostring(select('#', ...)) \
        for i = 1, select('#', ...) do s = s .. ',' .. tostring((select(i, ...))) end \
        return s end ";

    /// What one run of a case leaves behind: its result or error, the cost
    /// used and the next draw of the random stream, with the structural
    /// events when fault injection records them.
    fn observe(
        case: &str,
        forced: bool,
        budget: Option<i64>,
        fault: Option<Option<u32>>,
    ) -> String {
        // Leaves decline while inline-cache hits are off; the comparison
        // needs them on.
        let _hits = crate::vm::caches::HitMode::required();
        FORCE_CALLBACK.set(forced);
        let mut state = State::new();
        state.set_rng_seed(7);
        let source = format!("{SHOW} return show((function() {case} end)())");
        let program = crate::Program::compile(&source, None).expect("the case compiles");
        state.load(&program).expect("the case loads");
        if let Some(budget) = budget {
            state.set_cost_budget(budget).expect("the State is metered");
        }
        #[cfg(feature = "fault-injection")]
        if let Some(target) = fault {
            state.arm_fault_injection(target);
        }
        #[cfg(not(feature = "fault-injection"))]
        assert!(fault.is_none(), "fault injection is not built");
        let outcome = match state.call(ArgCount::Fixed(0), RetCount::Fixed(1)) {
            Ok(()) => String::from_utf8_lossy(state.to_bytes(-1).expect("a string")).into_owned(),
            Err(error) => format!("error {:?}: {error}", error.kind()),
        };
        #[cfg(feature = "fault-injection")]
        let events = if fault.is_some() {
            format!("{:?}", state.finish_fault_injection())
        } else {
            String::new()
        };
        #[cfg(not(feature = "fault-injection"))]
        let events = String::new();
        let cost = state.cost_used();
        state.set_top(0).expect("the stack clears");
        if budget.is_some() {
            state
                .set_cost_budget(i64::MAX)
                .expect("the State is metered");
        }
        let next = crate::Program::compile("return math.random()", None)
            .and_then(|program| state.load(&program))
            .and_then(|()| state.call(ArgCount::Fixed(0), RetCount::Fixed(1)))
            .and_then(|()| state.to_number(-1));
        FORCE_CALLBACK.set(false);
        format!("{outcome} / cost {cost} / next {next:?} / events {events}")
    }

    fn cost_of(observation: &str) -> i64 {
        observation
            .split(" / cost ")
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .expect("the observation carries the cost")
            .parse()
            .expect("the cost is an integer")
    }

    #[test]
    fn leaves_behave_as_their_callbacks() {
        for case in CASES {
            assert_eq!(
                observe(case, false, None, None),
                observe(case, true, None, None),
                "{case}"
            );
        }
    }

    /// Every budget that runs out somewhere inside a case fails it at the
    /// same operation, with the same cost and stream, on both paths.
    #[test]
    fn leaves_cross_the_budget_where_their_callbacks_do() {
        for case in CASES {
            let cost = cost_of(&observe(case, true, None, None));
            for budget in 0..=cost {
                assert_eq!(
                    observe(case, false, Some(budget), None),
                    observe(case, true, Some(budget), None),
                    "{case} with budget {budget}"
                );
            }
        }
    }

    /// Every structural refusal (stack-slot checks included) fires at the
    /// same event on both paths and leaves the same error, cost and random
    /// stream: a leaf checks the slots its callback pushed at, in its
    /// order, after the same draws.
    #[cfg(feature = "fault-injection")]
    #[test]
    fn leaves_refuse_where_their_callbacks_do() {
        for case in CASES {
            let baseline = observe(case, true, None, Some(None));
            assert_eq!(observe(case, false, None, Some(None)), baseline, "{case}");
            let eligible = baseline.matches("eligible: true").count();
            for target in 1..=u32::try_from(eligible).expect("few events") {
                assert_eq!(
                    observe(case, false, None, Some(Some(target))),
                    observe(case, true, None, Some(Some(target))),
                    "{case} refused at event {target}"
                );
            }
        }
    }
}
