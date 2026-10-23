//! Lua's Math Library
//!
//! `math.random` draws from the in-crate seeded `VmRng`, so a fixed seed yields
//! the same stream on every platform. `sin`, `cos`, `tan`, `asin`, `acos`,
//! `atan`, `atan2`, `exp` and `log` come from `crate::crmath`, in-crate ports
//! of the CORE-MATH correctly rounded functions that give the same bits on
//! every supported host (as does the `^` operator, whose opcode uses
//! `crmath::pow`); `docs/determinism.md` states the guarantee and
//! `reference/deterministic-math.md` how it is built. `sqrt` is correctly
//! rounded by IEEE 754. Basic arithmetic (`+ - * / %` and unary minus),
//! comparisons, `sqrt`, and `math.random` are bit-stable across platforms too.
//! The sign and payload of a NaN any of them produce are not (x86-64's
//! default NaN is negative, aarch64's positive), but no Lua operation observes
//! those bits: `string.format` and `tostring` both canonicalise NaN to an
//! unsigned `nan`, and `%q` prints `(0/0)`.

use super::{MATH_LIBRARY_CAPACITY, arg_integer, check_optional_type_named, check_type_named};
use crate::LuaType;
use crate::Result;
use crate::State;
use crate::crmath;
use crate::error::{ArgDetail, ErrorKind, LibraryError, LibraryFunction};
use std::f64::consts::PI;

// Every math function charges a flat 1 through its own `consume_cost(1)`.
// The repetition is deliberate: the cost is data-independent (no function here
// does work proportional to its inputs) and is part of the versioned cost
// model, so it stays visible beside each function rather than behind a shared
// helper that could drift per-function costs together by accident.
// Keep registration order stable: it determines table iteration order and
// snapshot baselines.
// The constants follow the functions in the math table; keep their order stable.
pub(super) const MATH_CONSTANTS: &[(&str, f64)] = &[
    // math.pi (constant)
    ("pi", PI),
    // math.huge (infinity constant)
    ("huge", f64::INFINITY),
];

pub(super) const MATH_FUNCTIONS: &[(&str, crate::RustFunc)] = &[
    // math.sin(x) - costs 1
    ("sin", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "sin")?;
        let x = state.to_number(1)?;
        state.set_top(0)?;
        state.push_number(crmath::sin(x))?;
        Ok(1)
    }),
    // math.cos(x) - costs 1
    ("cos", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "cos")?;
        let x = state.to_number(1)?;
        state.set_top(0)?;
        state.push_number(crmath::cos(x))?;
        Ok(1)
    }),
    // math.atan2(y, x) - costs 1
    ("atan2", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "atan2")?;
        check_type_named(state, 2, LuaType::Number, "atan2")?;
        let y = state.to_number(1)?;
        let x = state.to_number(2)?;
        state.set_top(0)?;
        state.push_number(crmath::atan2(y, x))?;
        Ok(1)
    }),
    // math.sqrt(x) - costs 1
    ("sqrt", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "sqrt")?;
        let x = state.to_number(1)?;
        state.set_top(0)?;
        state.push_number(x.sqrt())?;
        Ok(1)
    }),
    // math.abs(x) - costs 1
    ("abs", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "abs")?;
        let x = state.to_number(1)?;
        state.set_top(0)?;
        state.push_number(x.abs())?;
        Ok(1)
    }),
    // math.min(...) - costs 1, returns minimum of all arguments
    ("min", |state| {
        state.consume_cost(1)?;
        let n = state.top_isize();
        check_type_named(state, 1, LuaType::Number, "min")?;
        let mut result = state.to_number(1)?;
        for i in 2..=n {
            check_type_named(state, i, LuaType::Number, "min")?;
            let v = state.to_number(i)?;
            if v < result {
                result = v;
            }
        }
        state.set_top(0)?;
        state.push_number(result)?;
        Ok(1)
    }),
    // math.max(...) - costs 1, returns maximum of all arguments
    ("max", |state| {
        state.consume_cost(1)?;
        let n = state.top_isize();
        check_type_named(state, 1, LuaType::Number, "max")?;
        let mut result = state.to_number(1)?;
        for i in 2..=n {
            check_type_named(state, i, LuaType::Number, "max")?;
            let v = state.to_number(i)?;
            if v > result {
                result = v;
            }
        }
        state.set_top(0)?;
        state.push_number(result)?;
        Ok(1)
    }),
    // math.floor(x) - costs 1
    ("floor", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "floor")?;
        let x = state.to_number(1)?;
        state.set_top(0)?;
        state.push_number(x.floor())?;
        Ok(1)
    }),
    // math.ceil(x) - costs 1
    ("ceil", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "ceil")?;
        let x = state.to_number(1)?;
        state.set_top(0)?;
        state.push_number(x.ceil())?;
        Ok(1)
    }),
    // math.random() / math.random(n) / math.random(m, n) - costs 1
    ("random", |state| {
        state.consume_cost(1)?;
        let num_args = state.get_top();

        let result = match num_args {
            0 => {
                // math.random() - returns [0, 1)
                state.rng.random_f64()
            }
            1 => {
                // math.random(n) - returns integer in [1, n]
                let n = arg_integer(state, 1, "random")?;
                if n < 1 {
                    return Err(state.error(ErrorKind::argument_detail(
                        1,
                        "random",
                        ArgDetail::IntervalEmpty,
                    )));
                }
                state.rng.random_range_i64(1, n) as f64
            }
            2 => {
                // math.random(m, n) - returns integer in [m, n]
                let m = arg_integer(state, 1, "random")?;
                let n = arg_integer(state, 2, "random")?;
                if m > n {
                    // Argument #1 is deliberate and matches 5.4; only 5.2
                    // blames #2. Reading this against 5.2 makes it look like
                    // an off-by-one, so it has been "found" as a bug before -
                    // verified against both interpreters, and changing it to
                    // #2 would introduce a divergence rather than fix one.
                    return Err(state.error(ErrorKind::argument_detail(
                        1,
                        "random",
                        ArgDetail::IntervalEmpty,
                    )));
                }
                // Dellingr numbers are f64, so integer draws outside +-2^53
                // can round. Rejection removes bias from the i64 draw, but
                // the rounded f64 results need not be uniform.
                // It cannot leave [m, n]: both bounds came in as f64 (and
                // `arg_integer` rejects 2^63), so they are representable, and
                // round-to-nearest is monotonic. The i64 draw itself is
                // unbiased (rejection sampling in `VmRng::random_range_i64`).
                state.rng.random_range_i64(m, n) as f64
            }
            _ => {
                return Err(state.library_error(LibraryError::WrongNumberOfArguments {
                    function: LibraryFunction::Random,
                }));
            }
        };

        state.set_top(0)?;
        state.push_number(result)?;
        Ok(1)
    }),
    // math.tan(x) - costs 1
    ("tan", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "tan")?;
        let x = state.to_number(1)?;
        state.set_top(0)?;
        state.push_number(crmath::tan(x))?;
        Ok(1)
    }),
    // math.acos(x) - costs 1
    ("acos", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "acos")?;
        let x = state.to_number(1)?;
        state.set_top(0)?;
        state.push_number(crmath::acos(x))?;
        Ok(1)
    }),
    // math.asin(x) - costs 1
    ("asin", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "asin")?;
        let x = state.to_number(1)?;
        state.set_top(0)?;
        state.push_number(crmath::asin(x))?;
        Ok(1)
    }),
    // math.atan(y [, x]) - Lua 5.3+ style - costs 1
    // With one arg: returns atan(y)
    // With two args: returns atan2(y, x)
    ("atan", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "atan")?;
        let y = state.to_number(1)?;
        let result = if check_optional_type_named(state, 2, LuaType::Number, "atan")? {
            let x = state.to_number(2)?;
            crmath::atan2(y, x)
        } else {
            crmath::atan(y)
        };
        state.set_top(0)?;
        state.push_number(result)?;
        Ok(1)
    }),
    // math.deg(x) - radians to degrees - costs 1
    ("deg", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "deg")?;
        let x = state.to_number(1)?;
        state.set_top(0)?;
        state.push_number(x.to_degrees())?;
        Ok(1)
    }),
    // math.rad(x) - degrees to radians - costs 1
    ("rad", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "rad")?;
        let x = state.to_number(1)?;
        state.set_top(0)?;
        state.push_number(x.to_radians())?;
        Ok(1)
    }),
    // math.exp(x) - e^x - costs 1
    ("exp", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "exp")?;
        let x = state.to_number(1)?;
        state.set_top(0)?;
        state.push_number(crmath::exp(x))?;
        Ok(1)
    }),
    // math.log(x [, base]) - natural log, or log with base - costs 1
    ("log", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "log")?;
        let x = state.to_number(1)?;
        let result = if check_optional_type_named(state, 2, LuaType::Number, "log")? {
            let base = state.to_number(2)?;
            // Reference special-cases base 10 with log10, which makes
            // math.log(1000, 10) exactly 3 rather than 2.9999999999999996.
            // An exact comparison is what is wanted here: only the literal
            // base 10 takes that path, as in reference.
            // Any other base is the quotient of two correctly rounded natural
            // logarithms, rounded once more: deterministic, but not a
            // correctly rounded log_base.
            #[allow(clippy::float_cmp)]
            let is_base_ten = base == 10.0;
            if is_base_ten {
                crmath::log10(x)
            } else {
                crmath::ln(x) / crmath::ln(base)
            }
        } else {
            crmath::ln(x)
        };
        state.set_top(0)?;
        state.push_number(result)?;
        Ok(1)
    }),
    // math.fmod(x, y) - float modulo (same sign as x) - costs 1
    ("fmod", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "fmod")?;
        check_type_named(state, 2, LuaType::Number, "fmod")?;
        let x = state.to_number(1)?;
        let y = state.to_number(2)?;
        state.set_top(0)?;
        state.push_number(x % y)?;
        Ok(1)
    }),
    // math.modf(x) - returns integer and fractional parts - costs 1
    ("modf", |state| {
        state.consume_cost(1)?;
        check_type_named(state, 1, LuaType::Number, "modf")?;
        let x = state.to_number(1)?;
        let integral = x.trunc();
        let fractional = if matches!(x.partial_cmp(&integral), Some(std::cmp::Ordering::Equal)) {
            0.0
        } else {
            x - integral
        };
        state.set_top(0)?;
        state.push_number(integral)?;
        state.push_number(fractional)?;
        Ok(2)
    }),
];

pub(crate) fn open_math(state: &mut State) -> Result<()> {
    // Create the math table
    state.new_table_with_capacity(MATH_LIBRARY_CAPACITY)?;

    for &(name, func) in MATH_FUNCTIONS {
        state.register_lib_fn(-1, "math", name, func)?;
    }

    for &(name, value) in MATH_CONSTANTS {
        state.set_table_str_key_number(-1, name, value)?;
    }

    // Set the math table as a global
    state.set_global("math")?;
    Ok(())
}
