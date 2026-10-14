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
use crate::error::{Error, ErrorKind};
use crate::numeral::exact_i64;

/// Headroom the library setup needs on the value stack at any one moment.
///
/// `open_libs` is reachable from the public `State::open_libs`, so a host can
/// call it on a non-empty stack. Every library installs its entries by pushing
/// and immediately storing them, so the setup never holds more than a handful
/// of live values; the deepest point is `_G` in `open_base`, which holds the
/// table, its metatable, and a key/value pair - four slots. Reserving up front
/// keeps the operation all-or-nothing: without it, a stack near the cap could
/// install part of the standard library (and, under `snapshot`, part of the
/// function registry) before failing partway through.
///
/// The per-push checks remain the backstop if a future library ever exceeds
/// this; they would report the overflow correctly, just without the
/// all-or-nothing guarantee.
const STDLIB_SETUP_HEADROOM: usize = 8;

pub(crate) fn open_libs(state: &mut State) -> Result<()> {
    state.check_stack_space(STDLIB_SETUP_HEADROOM)?;
    state.reserve_stdlib_capacity();
    open_base(state)?;
    math::open_math(state)?;
    string::open_string(state)?;
    table::open_table(state)
}

// Do not add module-wide cast denies until the remaining bounded index and
// result conversions here have been audited or replaced.

/// Formats a standard-library argument error with a named function and detail.
///
/// `ArgError` currently represents only one expected Lua type, so errors for
/// a value with the right type but an invalid value need this runtime-error
/// path until the error type can carry a structured detail.
pub(super) fn argument_error(
    state: &State,
    arg_number: isize,
    function_name: &str,
    detail: &str,
) -> Error {
    state.error(ErrorKind::RuntimeError(format!(
        "bad argument #{arg_number} to '{function_name}' ({detail})"
    )))
}

/// Converts a numeric argument to an exact i64, reporting its argument name.
pub(super) fn exact_integer_value(
    state: &State,
    number: f64,
    arg_number: isize,
    function_name: &str,
) -> Result<i64> {
    exact_i64(number).ok_or_else(|| {
        argument_error(
            state,
            arg_number,
            function_name,
            "number has no integer representation",
        )
    })
}

/// Checks a required numeric argument and converts it to an exact i64.
pub(super) fn arg_integer(
    state: &mut State,
    arg_number: isize,
    function_name: &str,
) -> Result<i64> {
    state.check_type(arg_number, LuaType::Number)?;
    let number = state.to_number(arg_number)?;
    exact_integer_value(state, number, arg_number, function_name)
}

/// Checks an optional numeric argument and converts it to an exact i64.
pub(super) fn opt_integer(
    state: &mut State,
    arg_number: isize,
    function_name: &str,
) -> Result<Option<i64>> {
    if state.check_optional_type(arg_number, LuaType::Number)? {
        Ok(Some(arg_integer(state, arg_number, function_name)?))
    } else {
        Ok(None)
    }
}

/// Implements both global `unpack` and `table.unpack`.
pub(super) fn unpack_values(state: &mut State) -> Result<u8> {
    state.check_type(1, LuaType::Table)?;
    // A table length beyond i64::MAX is unreachable; saturating keeps this infallible.
    let len = i64::try_from(state.table_len(1)).unwrap_or(i64::MAX);

    let i = opt_integer(state, 2, "unpack")?.unwrap_or(1);
    let j = opt_integer(state, 3, "unpack")?.unwrap_or(len);

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
        state.push_number(index as f64)?;
        state.get_table(1)?;
    }
    state.remove(1)?;
    Ok((span + 1) as u8)
}
