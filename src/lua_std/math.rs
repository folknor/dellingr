//! Lua's Math Library
//!
//! Determinism contract (L5): `math.random` is fully deterministic - it draws
//! from the in-crate seeded `VmRng`, so a fixed seed yields the same stream on
//! every platform. The transcendental functions (`sin`, `cos`, `exp`, `log`,
//! `pow`, `sqrt`, ...) delegate to the platform's `f64` / libm implementation.
//! Their results are deterministic for a given build+platform, but the last
//! ULP may differ across architectures or libm versions. Replays that must be
//! bit-identical across heterogeneous hosts should therefore avoid relying on
//! exact transcendental results (basic arithmetic, comparisons, and
//! `math.random` are bit-stable everywhere).

use crate::LuaType;
use crate::State;
use crate::error::{ArgError, ErrorKind};
use std::f64::consts::PI;

pub(crate) fn open_math(state: &mut State) {
    // Create the math table
    state.new_table_with_capacity(24);

    // Helper to add a function to the table at stack index -1.
    macro_rules! add_fn {
        ($name:expr, $func:expr) => {
            #[cfg(feature = "snapshot")]
            state
                .set_table_str_key_named_rust_fn(-1, $name, concat!("math.", $name), $func)
                .expect("math library registration cannot fail");
            #[cfg(not(feature = "snapshot"))]
            state
                .set_table_str_key_rust_fn(-1, $name, $func)
                .expect("math library registration cannot fail");
        };
    }

    // math.sin(x) - costs 1
    add_fn!("sin", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        let x = state.to_number(1)?;
        state.set_top(0);
        state.push_number(x.sin());
        Ok(1)
    });

    // math.cos(x) - costs 1
    add_fn!("cos", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        let x = state.to_number(1)?;
        state.set_top(0);
        state.push_number(x.cos());
        Ok(1)
    });

    // math.atan2(y, x) - costs 1
    add_fn!("atan2", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        state.check_type(2, LuaType::Number)?;
        let y = state.to_number(1)?;
        let x = state.to_number(2)?;
        state.set_top(0);
        state.push_number(y.atan2(x));
        Ok(1)
    });

    // math.sqrt(x) - costs 1
    add_fn!("sqrt", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        let x = state.to_number(1)?;
        state.set_top(0);
        state.push_number(x.sqrt());
        Ok(1)
    });

    // math.abs(x) - costs 1
    add_fn!("abs", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        let x = state.to_number(1)?;
        state.set_top(0);
        state.push_number(x.abs());
        Ok(1)
    });

    // math.min(...) - costs 1, returns minimum of all arguments
    add_fn!("min", |state| {
        state.consume_cost(1)?;
        let n = state.get_top() as isize;
        if n == 0 {
            let e = ArgError {
                arg_number: 1,
                func_name: Some("min".to_string()),
                expected: Some(LuaType::Number),
                received: None,
            };
            return Err(state.error(ErrorKind::ArgError(e)));
        }
        state.check_type(1, LuaType::Number)?;
        let mut result = state.to_number(1)?;
        for i in 2..=n {
            state.check_type(i, LuaType::Number)?;
            let v = state.to_number(i)?;
            if v < result {
                result = v;
            }
        }
        state.set_top(0);
        state.push_number(result);
        Ok(1)
    });

    // math.max(...) - costs 1, returns maximum of all arguments
    add_fn!("max", |state| {
        state.consume_cost(1)?;
        let n = state.get_top() as isize;
        if n == 0 {
            let e = ArgError {
                arg_number: 1,
                func_name: Some("max".to_string()),
                expected: Some(LuaType::Number),
                received: None,
            };
            return Err(state.error(ErrorKind::ArgError(e)));
        }
        state.check_type(1, LuaType::Number)?;
        let mut result = state.to_number(1)?;
        for i in 2..=n {
            state.check_type(i, LuaType::Number)?;
            let v = state.to_number(i)?;
            if v > result {
                result = v;
            }
        }
        state.set_top(0);
        state.push_number(result);
        Ok(1)
    });

    // math.floor(x) - costs 1
    add_fn!("floor", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        let x = state.to_number(1)?;
        state.set_top(0);
        state.push_number(x.floor());
        Ok(1)
    });

    // math.ceil(x) - costs 1
    add_fn!("ceil", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        let x = state.to_number(1)?;
        state.set_top(0);
        state.push_number(x.ceil());
        Ok(1)
    });

    // math.random() / math.random(n) / math.random(m, n) - costs 1
    add_fn!("random", |state| {
        state.consume_cost(1)?;
        let num_args = state.get_top();

        let result = match num_args {
            0 => {
                // math.random() - returns [0, 1)
                state.rng.random_f64()
            }
            1 => {
                // math.random(n) - returns integer in [1, n]
                state.check_type(1, LuaType::Number)?;
                let n = state.to_number(1)? as i64;
                if n < 1 {
                    return Err(state.error(ErrorKind::RuntimeError(
                        "bad argument #1 to 'random' (interval is empty)".to_string(),
                    )));
                }
                state.rng.random_range_i64(1, n) as f64
            }
            2 => {
                // math.random(m, n) - returns integer in [m, n]
                state.check_type(1, LuaType::Number)?;
                state.check_type(2, LuaType::Number)?;
                let m = state.to_number(1)? as i64;
                let n = state.to_number(2)? as i64;
                if m > n {
                    return Err(state.error(ErrorKind::RuntimeError(
                        "bad argument #1 to 'random' (interval is empty)".to_string(),
                    )));
                }
                state.rng.random_range_i64(m, n) as f64
            }
            _ => {
                return Err(state.error(ErrorKind::RuntimeError(
                    "wrong number of arguments to 'random'".to_string(),
                )));
            }
        };

        state.set_top(0);
        state.push_number(result);
        Ok(1)
    });

    // math.tan(x) - costs 1
    add_fn!("tan", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        let x = state.to_number(1)?;
        state.set_top(0);
        state.push_number(x.tan());
        Ok(1)
    });

    // math.acos(x) - costs 1
    add_fn!("acos", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        let x = state.to_number(1)?;
        state.set_top(0);
        state.push_number(x.acos());
        Ok(1)
    });

    // math.asin(x) - costs 1
    add_fn!("asin", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        let x = state.to_number(1)?;
        state.set_top(0);
        state.push_number(x.asin());
        Ok(1)
    });

    // math.atan(y [, x]) - Lua 5.3+ style - costs 1
    // With one arg: returns atan(y)
    // With two args: returns atan2(y, x)
    add_fn!("atan", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        let y = state.to_number(1)?;
        let result = if state.get_top() >= 2 {
            state.check_type(2, LuaType::Number)?;
            let x = state.to_number(2)?;
            y.atan2(x)
        } else {
            y.atan()
        };
        state.set_top(0);
        state.push_number(result);
        Ok(1)
    });

    // math.deg(x) - radians to degrees - costs 1
    add_fn!("deg", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        let x = state.to_number(1)?;
        state.set_top(0);
        state.push_number(x.to_degrees());
        Ok(1)
    });

    // math.rad(x) - degrees to radians - costs 1
    add_fn!("rad", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        let x = state.to_number(1)?;
        state.set_top(0);
        state.push_number(x.to_radians());
        Ok(1)
    });

    // math.exp(x) - e^x - costs 1
    add_fn!("exp", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        let x = state.to_number(1)?;
        state.set_top(0);
        state.push_number(x.exp());
        Ok(1)
    });

    // math.log(x [, base]) - natural log, or log with base - costs 1
    add_fn!("log", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        let x = state.to_number(1)?;
        let result = if state.get_top() >= 2 {
            state.check_type(2, LuaType::Number)?;
            let base = state.to_number(2)?;
            x.log(base)
        } else {
            x.ln()
        };
        state.set_top(0);
        state.push_number(result);
        Ok(1)
    });

    // math.fmod(x, y) - float modulo (same sign as x) - costs 1
    add_fn!("fmod", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        state.check_type(2, LuaType::Number)?;
        let x = state.to_number(1)?;
        let y = state.to_number(2)?;
        state.set_top(0);
        state.push_number(x % y);
        Ok(1)
    });

    // math.modf(x) - returns integer and fractional parts - costs 1
    add_fn!("modf", |state| {
        state.consume_cost(1)?;
        state.check_type(1, LuaType::Number)?;
        let x = state.to_number(1)?;
        let integral = x.trunc();
        let fractional = if matches!(x.partial_cmp(&integral), Some(std::cmp::Ordering::Equal)) {
            0.0
        } else {
            x - integral
        };
        state.set_top(0);
        state.push_number(integral);
        state.push_number(fractional);
        Ok(2)
    });

    // math.pi (constant)
    state
        .set_table_str_key_number(-1, "pi", PI)
        .expect("math.pi assignment cannot fail");

    // math.huge (infinity constant)
    state
        .set_table_str_key_number(-1, "huge", f64::INFINITY)
        .expect("math.huge assignment cannot fail");

    // Set the math table as a global
    state.set_global("math");
}
