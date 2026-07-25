//! Lua's standard library

mod basic;
mod math;
mod string;
mod string_format;
mod table;

pub(crate) use basic::{base_ipairs_iter, base_next, open_base};

use crate::LuaType;
use crate::Result;
use crate::State;
use crate::error::ErrorKind;
use crate::numeral::exact_i64;

pub(crate) fn open_libs(state: &mut State) {
    state.reserve_stdlib_capacity();
    open_base(state);
    math::open_math(state);
    string::open_string(state);
    table::open_table(state);
}

/// Converts an already type-checked numeric library argument to an exact i64.
pub(super) fn exact_integer_argument(
    state: &mut State,
    arg_number: isize,
    function_name: &str,
) -> Result<i64> {
    let number = state.to_number(arg_number)?;
    exact_i64(number).ok_or_else(|| {
        state.error(ErrorKind::RuntimeError(format!(
            "bad argument #{arg_number} to '{function_name}' (number has no integer representation)"
        )))
    })
}

/// Implements both global `unpack` and `table.unpack`.
pub(super) fn unpack_values(state: &mut State) -> Result<u8> {
    state.check_type(1, LuaType::Table)?;
    let len = state.table_len(1) as i64;

    let i = if state.check_optional_type(2, LuaType::Number)? {
        exact_integer_argument(state, 2, "unpack")?
    } else {
        1
    };
    let j = if state.check_optional_type(3, LuaType::Number)? {
        exact_integer_argument(state, 3, "unpack")?
    } else {
        len
    };

    state.set_top(1)?;
    if i > j {
        state.set_top(0)?;
        return Ok(0);
    }
    let span = j
        .checked_sub(i)
        .ok_or_else(|| state.error(ErrorKind::RuntimeError("too many results to unpack".into())))?;
    if span >= 255 {
        return Err(state.error(ErrorKind::RuntimeError("too many results to unpack".into())));
    }
    for index in i..=j {
        state.push_number(index as f64);
        state.get_table(1)?;
    }
    state.remove(1)?;
    Ok((span + 1) as u8)
}
