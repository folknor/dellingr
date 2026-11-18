//! Library leaves: standard-library functions the dispatch loop executes
//! directly when Lua calls them, instead of entering them as Rust callbacks.
//!
//! A leaf is a catalog entry (`lua_std::leaf_of`) whose success path runs no
//! host code and no Lua: a computation on its arguments, or a raw table
//! write (`rawset`, raw `table.insert` and `table.remove`, `table.pack`), or
//! a cut of its own arguments (`select`). It needs
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
//! - The arguments stay in place above the callee slot instead of moving
//!   down over it, so the stack is one value longer than the callback's
//!   until the callee slot is reused. Every check the callback makes runs
//!   at the callback's length (`check_stack_slot_at`,
//!   `check_stack_space_at`), and the physical stack never exceeds the
//!   longest the callback's reached, so the stack cap refuses at the same
//!   operation. A value a leaf allocates goes into the callee slot, where it
//!   is rooted beside the arguments, as the callback's push roots it.
//! - Argument guards before the commit may set floating-point status flags
//!   (a NaN key compare, an exact-integer conversion) that the callback,
//!   refused at its charge first, would not have; the VM ignores status
//!   flags (`fp_env`), so that is not a difference.
//! - The computations the callbacks and leaves share live once, in the
//!   library (`lua_std::math`).
//!
//! Dedicated guarded library opcodes (Luau's FASTCALL) were weighed against
//! this identity path and not pursued: over it they would skip only the
//! call-site record, the call window and one out-of-line call, the cost
//! flush before the call must stay (folding the library charge into the
//! batched opcode cost would move a budget failure), and they would change
//! persisted opcode encodings. A `table.insert` or `table.remove` on a table
//! with sequence metamethods stays a callback: it calls Lua.
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
    RawSet,
    Select,
    TableInsert,
    TableRemove,
    TablePack,
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
            Leaf::RawSet => return self.leaf_rawset(idx, n),
            Leaf::Select => return self.leaf_select(idx, n),
            Leaf::TableInsert => return self.leaf_insert(idx, n),
            Leaf::TableRemove => return self.leaf_remove(idx, n),
            Leaf::TablePack => return self.leaf_pack(idx, n),
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

    /// `rawset(t, k, v)`: the key goes to the table as given (a first store
    /// under `-0.0` keeps that spelling for iteration), and the editor is
    /// taken after the charge even for a store that changes nothing or
    /// fails to grow, as the callback's `set_table_raw` takes it, so a store
    /// into `_G` moves the watched epoch on both paths.
    fn leaf_rawset(&mut self, idx: usize, n: usize) -> Result<Option<u8>> {
        if n < 3 {
            return Ok(None);
        }
        let table = self.stack[idx + 1];
        let Some(ptr) = table
            .as_object_ptr()
            .filter(|&ptr| self.heap.as_table_ref(ptr).is_some())
        else {
            return Ok(None);
        };
        let (key, value) = (self.stack[idx + 2], self.stack[idx + 3]);
        if key.table_key().is_err() {
            return Ok(None);
        }
        self.enter_leaf(Some(Charge::Rawset))?;
        self.heap
            .edit_table(ptr)
            .expect("the argument is a table")
            .insert(key, value)?;
        self.stack[idx] = table;
        self.stack.truncate(idx + 1);
        Ok(Some(1))
    }

    /// `select`: the count form pushes one number, checked where the
    /// callback's push is; the index form keeps the suffix of its arguments,
    /// moved down once as the callback's results are, and checks nothing.
    fn leaf_select(&mut self, idx: usize, n: usize) -> Result<Option<u8>> {
        if n == 0 {
            return Ok(None);
        }
        let selector = self.stack[idx + 1];
        let varargs = n - 1;
        if selector
            .as_string(&self.heap)
            .is_some_and(|bytes| bytes.first() == Some(&b'#'))
        {
            self.enter_leaf(None)?;
            #[expect(
                clippy::cast_precision_loss,
                reason = "select reports a count as a Lua number, as its callback does."
            )]
            let count = Val::Num(varargs as f64);
            self.leaf_result(idx, count)?;
            return Ok(Some(1));
        }
        let Some(raw) = (match selector {
            Val::Num(x) => exact_i64(x),
            _ => None,
        }) else {
            return Ok(None);
        };
        // At most 254 varargs: the call passed at most 255 arguments.
        let Ok(varargs_u8) = u8::try_from(varargs) else {
            return Ok(None);
        };
        let varargs_i64 = i64::from(varargs_u8);
        let resolved = if raw < 0 { varargs_i64 + raw + 1 } else { raw };
        if raw == 0 || (raw < 0 && resolved < 1) {
            return Ok(None);
        }
        if raw > varargs_i64 {
            self.enter_leaf(None)?;
            self.stack.truncate(idx);
            return Ok(Some(0));
        }
        // 1 <= resolved <= varargs here, so both conversions succeed.
        let (Ok(index), Ok(count)) = (
            usize::try_from(resolved),
            u8::try_from(varargs_i64 - resolved + 1),
        ) else {
            return Ok(None);
        };
        self.enter_leaf(None)?;
        self.stack.copy_within(idx + 1 + index..idx + 1 + n, idx);
        self.stack.truncate(idx + usize::from(count));
        Ok(Some(count))
    }

    /// A position argument as the callbacks' `arg_integer` reads it on
    /// success: a number holding an exact non-negative integer.
    fn exact_position(value: Val) -> Option<usize> {
        let Val::Num(x) = value else {
            return None;
        };
        // A negative integer is not a position: the caller declines.
        let Ok(pos) = usize::try_from(exact_i64(x)?) else {
            return None;
        };
        Some(pos)
    }

    /// The table argument of `table.insert` or `table.remove` when the raw
    /// storage path applies (a table whose metatable, if any, defines none
    /// of `__len`, `__index`, `__newindex`), with its border.
    fn plain_sequence(&self, value: Val) -> Option<(super::object::ObjectPtr, usize)> {
        let ptr = value.as_object_ptr()?;
        let table = self.heap.as_table_ref(ptr)?;
        if self.has_sequence_metafields(table.get_metatable()) {
            return None;
        }
        Some((ptr, table.array_len()))
    }

    /// `table.insert` on the raw path: one storage shift, no pushes.
    fn leaf_insert(&mut self, idx: usize, n: usize) -> Result<Option<u8>> {
        if n != 2 && n != 3 {
            return Ok(None);
        }
        let Some((ptr, len)) = self.plain_sequence(self.stack[idx + 1]) else {
            return Ok(None);
        };
        let pos = if n == 2 {
            len + 1
        } else {
            match Self::exact_position(self.stack[idx + 2]) {
                Some(pos) if (1..=len + 1).contains(&pos) => pos,
                _ => return Ok(None),
            }
        };
        let value = self.stack[idx + n];
        self.enter_leaf(Some(Charge::TableInsert))?;
        self.heap
            .edit_table(ptr)
            .expect("the argument is a table")
            .array_insert_at_len(pos, len, value)?;
        self.stack.truncate(idx);
        Ok(Some(0))
    }

    /// `table.remove` on the raw path. The callback reserves its result
    /// slot before the removal, with its stack cut to the table alone; the
    /// leaf runs that check at the same length, one below its own.
    fn leaf_remove(&mut self, idx: usize, n: usize) -> Result<Option<u8>> {
        if n == 0 {
            return Ok(None);
        }
        let Some((ptr, len)) = self.plain_sequence(self.stack[idx + 1]) else {
            return Ok(None);
        };
        let pos = match (n > 1).then(|| self.stack[idx + 2]) {
            None | Some(Val::Nil) => len,
            Some(value) => match Self::exact_position(value) {
                Some(pos)
                    if (len == 0 && pos <= 1) || (len > 0 && (1..=len + 1).contains(&pos)) =>
                {
                    pos
                }
                _ => return Ok(None),
            },
        };
        self.enter_leaf(Some(Charge::TableRemove))?;
        self.stack.truncate(idx + 2);
        self.check_stack_space_at(idx + 1, 1)?;
        let removed = self
            .heap
            .edit_table(ptr)
            .expect("the argument is a table")
            .array_remove_at_len(pos, len)?;
        self.stack[idx] = removed;
        self.stack.truncate(idx + 1);
        Ok(Some(1))
    }

    /// `table.pack`: the arguments stay in place and the new table goes to
    /// the callee slot (`State::pack_values`), saving the callback's
    /// argument move.
    fn leaf_pack(&mut self, idx: usize, n: usize) -> Result<Option<u8>> {
        self.enter_leaf(Some(Charge::TablePack))?;
        self.pack_values(idx + 1, n, Some(idx))?;
        self.stack.truncate(idx + 1);
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
        "local t = {} local r = rawset(t, 'a', 1) rawset(t, 1, 2) rawset(t, -0.0, 3) rawset(t, 'a', nil) local k = next(t) return r == t, t.a, t[1], k, 1 / k, rawset(t, 2, 4, 5) == t",
        "local z = {} rawset(z, -0.0, 1) local k = next(z) return 1 / k",
        "rawset(_G, 'leaf_g', 7) local v = leaf_g rawset(_G, 'leaf_g', 8) return v, leaf_g",
        "local p = print local a = type(print) rawset(_G, 'print', false) local b = print rawset(_G, 'print', p) return a, b, print == p",
        "return table.remove(1)",
        "return table.remove({1, 2}, 1.5)",
        "local t = setmetatable({1}, {__newindex = rawset}) table.insert(t, 2) return table.remove(t), #t",
        "local o = {i = table.insert, r = table.remove, s = select} return o:i('bad', 1)",
        "local o = {r = table.remove} return o:r('x')",
        "local o = {s = select} return o.s(o, 1)",
        "local t = setmetatable({}, {__newindex = function() error('no') end}) rawset(t, 'x', 1) return t.x",
        "return rawset({}, nil, 1)",
        "return rawset({}, 0/0, 1)",
        "return rawset({}, 'k')",
        "return rawset(1, 'k', 2)",
        "return select('#'), select('#', 1, nil, 3), select('#x', nil), select(2, 'a', 'b', 'c'), select(-1, 'a', 'b'), select(5, 'a')",
        "local a, b = select(2, 'x', 'y', nil, 'z') local c = select(3, 1, 2) return a, b, c, select(1, nil, nil)",
        "local function f(...) return select('#', ...), select(-2, ...) end return f(1, 2, 3)",
        "local function f(...) return select(2, ...) end return f(1, 2, 3)",
        "return select()",
        "return select(0, 1)",
        "return select(-3, 1)",
        "return select(1.5, 1)",
        "return select('2', 'a', 'b')",
        "local t = {} table.insert(t, 'a') table.insert(t, 1, 'b') table.insert(t, 3, 'c') local r = table.insert(t, 'd') return r, #t, t[1], t[2], t[3], t[4], table.concat(t)",
        "local t = setmetatable({1}, {__tostring = tostring}) table.insert(t, 2) return #t, t[2]",
        "local t = setmetatable({}, {__len = false}) table.insert(t, 1) return rawlen(t)",
        "local t = {1, 2} table.insert(t, 5, 3) return #t",
        "local t = {1, 2} table.insert(t, 0, 3) return #t",
        "local t = {1, 2} table.insert(t, 1.5, 3) return #t",
        "local t = {1, 2} table.insert(t, '1', 3) return #t",
        "return table.insert({})",
        "return table.insert({}, 1, 2, 3)",
        "return table.insert(1, 2)",
        "local t = {1, 2, 3, 4} local a = table.remove(t) local b = table.remove(t, 1) local c = table.remove(t, #t + 1) local d = table.remove(t, nil) return a, b, c, d, #t, t[1]",
        "local t = {[0] = 'x'} return table.remove(t), t[0], table.remove({}), table.remove({}, 0), table.remove({}, 1), table.remove({1}, 1, 'extra')",
        "local function f(t) return table.remove(t) end return f({5, 6})",
        "local t = setmetatable({1, 2}, {__index = function() return 9 end}) return table.remove(t), #t",
        "return table.remove({1}, 3)",
        "return table.remove({}, 2)",
        "return table.remove({1}, 0)",
        "return table.remove({1}, 'x')",
        "return table.remove()",
        "local p = table.pack() local q = table.pack(1, nil, 3) local r = table.pack(nil) return p.n, q.n, q[1], q[2], q[3], #q, r.n, next(table.pack())",
        "local function f(...) return table.pack(...) end local p = f('a', 'b') return p.n, p[1], p[2]",
        "local o = {p = table.pack, s = select, i = table.insert, r = table.remove, w = rawset} local t = {} o:i(1) o.i(t, 2) return o:p(1).n, o.s(1, o), o.r(t), o:w(1, 2) == o",
        "local o = {s = select} return o:s(1)",
        "local o = {i = table.insert} return o:i(1, 2, 3)",
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
