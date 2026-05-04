//! Lua's Math Library

use crate::LuaType;
use crate::State;
use crate::error::{ArgError, ErrorKind};
use rand::RngExt;
use std::f64::consts::PI;

pub(crate) fn open_math(state: &mut State) {
    // Create the math table
    state.new_table();

    // Helper to add a function to the table at stack index -1
    // Stack order: value first, then key (set_table_raw pops key, then value)
    macro_rules! add_fn {
        ($name:expr, $func:expr) => {
            state.push_rust_fn($func);
            state.push_string($name.to_string());
            state.set_table_raw(-3).unwrap();
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
                state.rng.random::<f64>()
            }
            1 => {
                // math.random(n) - returns integer in [1, n]
                state.check_type(1, LuaType::Number)?;
                let n = state.to_number(1)? as i64;
                if n < 1 {
                    state.set_top(0);
                    state.push_nil();
                    return Ok(1);
                }
                state.rng.random_range(1..=n) as f64
            }
            _ => {
                // math.random(m, n) - returns integer in [m, n]
                state.check_type(1, LuaType::Number)?;
                state.check_type(2, LuaType::Number)?;
                let m = state.to_number(1)? as i64;
                let n = state.to_number(2)? as i64;
                if m > n {
                    state.set_top(0);
                    state.push_nil();
                    return Ok(1);
                }
                state.rng.random_range(m..=n) as f64
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
        state.set_top(0);
        state.push_number(x.trunc());
        state.push_number(x.fract());
        Ok(2)
    });

    // math.pi (constant)
    state.push_number(PI);
    state.push_string("pi".to_string());
    state
        .set_table_raw(-3)
        .expect("math.pi assignment cannot fail");

    // math.huge (infinity constant)
    state.push_number(f64::INFINITY);
    state.push_string("huge".to_string());
    state
        .set_table_raw(-3)
        .expect("math.huge assignment cannot fail");

    // Set the math table as a global
    state.set_global("math");
}
