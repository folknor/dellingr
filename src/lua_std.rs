//! Lua's standard library.
//!
//! Version-sensitive compatibility decisions are per function: `string.format`
//! follows Lua 5.4, `math.random` follows Lua 5.4's interval and error behavior,
//! `math.log(x, base)` follows Lua 5.2's general-base calculation, and
//! `math.atan(y, x)` uses the Lua 5.3+ two-argument form. Global `unpack` is kept
//! from Lua 5.2 as an alias, but it and `table.unpack` share one implementation
//! with Lua 5.4 semantics. The table library (`insert`, `remove`, `concat`,
//! `unpack`, `move`, `sort`) follows Lua 5.4's metamethod rules: lengths come from
//! `__len`, reads go through `__index` and writes through `__newindex`, where
//! Lua 5.2 reads raw. Sort handler calls have a different order from Lua 5.4.
//! The `%p` conversion in
//! `string.format` follows Lua 5.4 and is absent in Lua 5.2.
//! `string.gsub` and `string.gmatch` follow Lua 5.4's empty-match rule: an
//! empty match ending where the previous match ended is skipped.
//! `string.gmatch` also accepts Lua 5.4's optional `init` argument.

#![deny(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

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

/// Capacity reservations for the standard-library setup path.
///
/// Keep the per-library counts aligned with all entries added to each table.
/// Math includes its `pi` and `huge` constants. The setup preflight reserves
/// four slots before any library has been installed.
const MATH_LIBRARY_CAPACITY: usize = 22;
const STRING_LIBRARY_CAPACITY: usize = 10;
const TABLE_LIBRARY_CAPACITY: usize = table::TABLE_FUNCTIONS.len();

/// Headroom the library setup reserves on the value stack.
///
/// `open_libs` is reachable from the public `State::open_libs`, so the
/// preflight accounts for values already on the stack. The largest setup
/// overlap is `_G` in `open_base`: its table, metatable, `__metatable` key
/// and protection value occupy exactly four slots at once.
/// Reserving up front keeps the operation all-or-nothing: without it, a stack
/// near the cap could install part of the standard library (and, under
/// `snapshot`, part of the function registry) before failing partway through.
/// Under `snapshot`, a separate registry preflight also rejects conflicting
/// registrations before setup, extending the guarantee to the function registry.
///
/// The per-push checks remain the backstop if a future library ever exceeds
/// this; they would report the overflow correctly, just without the
/// all-or-nothing guarantee.
const STDLIB_SETUP_HEADROOM: usize = 4;

pub(crate) fn open_libs(state: &mut State) -> Result<()> {
    state.check_stack_space(STDLIB_SETUP_HEADROOM)?;
    // Runs before any mutation, so a rejected call leaves the State untouched.
    #[cfg(feature = "snapshot")]
    check_registry_compatible(state)?;
    open_libs_unchecked(state)
}

/// The installation sequence itself, with no preflight. Shared by `open_libs`
/// and the registry dry run, so the dry run cannot drift from the real list of
/// ids and functions.
fn open_libs_unchecked(state: &mut State) -> Result<()> {
    state.reserve_stdlib_capacity();
    open_base(state)?;
    math::open_math(state)?;
    string::open_string(state)?;
    table::open_table(state)
}

/// Dry run for the snapshot function registry.
///
/// A host that registered an id the standard library also uses (`"print"`,
/// `"string.find"`, `"string.gmatch.iter"`, ...) for a different function would
/// otherwise hit `DuplicateFunctionRegistration` partway through setup, with
/// earlier libraries already installed as globals and their ids registered. The
/// host can still use the State after that error (`State::empty` followed by
/// `register_rust_fn` and `open_libs` is the supported route), so it must be
/// all-or-nothing.
///
/// The standard library is opened on a scratch State, and every (id, function)
/// pair it registered is compared with `state`'s registry. Re-registering the
/// same id to the same function is idempotent, so reopening the libraries on a
/// State that already has them passes. A State with an empty registry cannot
/// conflict and skips the scratch build.
/// Setup only installs values and registers function pointers: it executes no
/// Lua or callbacks, including the lazily compiled gmatch wrapper. The scratch
/// State owns its heap and registry; construction advances the anchor state-id
/// counter but does not draw from the caller's RNG or alter its environment.
#[cfg(feature = "snapshot")]
fn check_registry_compatible(state: &State) -> Result<()> {
    if state.rust_fns_by_id.is_empty() {
        return Ok(());
    }
    let mut scratch = State::empty();
    open_libs_unchecked(&mut scratch)?;
    for (id, func) in &scratch.rust_fns_by_id {
        if let Some(existing) = state.rust_fns_by_id.get(id)
            && *existing as usize != *func as usize
        {
            return Err(Error::without_location(
                ErrorKind::DuplicateFunctionRegistration { id: id.clone() },
            ));
        }
    }
    Ok(())
}

impl State {
    // This stdlib helper centralizes the cfg choices. Enforcing the boundary by
    // visibility requires moving it into vm before narrowing the table writers
    // there to pub(super).
    /// Installs a standard-library function and registers its stable snapshot id.
    pub(super) fn register_lib_fn(
        &mut self,
        table_idx: isize,
        lib_prefix: &str,
        name: &str,
        func: crate::RustFunc,
    ) -> Result<()> {
        #[cfg(feature = "snapshot")]
        {
            let id = format!("{lib_prefix}.{name}");
            self.set_table_str_key_named_rust_fn(table_idx, name, &id, func)
        }
        #[cfg(not(feature = "snapshot"))]
        {
            let _ = lib_prefix;
            self.set_table_str_key_rust_fn(table_idx, name, func)
        }
    }

    /// Registers a standard-library function id without storing the function.
    pub(super) fn register_lib_fn_id(&mut self, id: &str, func: crate::RustFunc) -> Result<()> {
        #[cfg(feature = "snapshot")]
        self.register_rust_fn(id, func)
            .map_err(Self::registration_error)?;
        #[cfg(not(feature = "snapshot"))]
        let _ = (id, func);
        Ok(())
    }

    /// Pushes a standard-library function using the active snapshot policy.
    pub(super) fn push_lib_fn(&mut self, id: &str, func: crate::RustFunc) -> Result<()> {
        #[cfg(feature = "snapshot")]
        self.push_named_rust_fn(id, func)?;
        #[cfg(not(feature = "snapshot"))]
        {
            let _ = id;
            self.push_rust_fn(func)?;
        }
        Ok(())
    }

    /// Installs a standard-library global and registers its stable snapshot id.
    pub(super) fn register_lib_global_fn(
        &mut self,
        name: &str,
        func: crate::RustFunc,
    ) -> Result<()> {
        #[cfg(feature = "snapshot")]
        self.set_global_named_rust_fn(name, name, func)
            .map_err(Self::registration_error)?;
        #[cfg(not(feature = "snapshot"))]
        self.set_global_rust_fn(name, func);
        Ok(())
    }
}

/// Adds the known standard-library function name to a generic argument error.
///
/// Explicit library names override global aliases and snapshot registration ids.
fn name_argument_error(mut error: Error, function_name: &str) -> Error {
    if let ErrorKind::ArgError(argument) = &mut error.kind {
        argument.func_name = Some(function_name.to_owned());
    }
    error
}

pub(super) fn check_any_named(
    state: &mut State,
    arg_number: isize,
    function_name: &str,
) -> Result<()> {
    state
        .check_any(arg_number)
        .map_err(|error| name_argument_error(error, function_name))
}

pub(super) fn check_type_named(
    state: &mut State,
    arg_number: isize,
    expected_type: LuaType,
    function_name: &str,
) -> Result<()> {
    state
        .check_type(arg_number, expected_type)
        .map_err(|error| name_argument_error(error, function_name))
}

pub(super) fn check_optional_type_named(
    state: &mut State,
    arg_number: isize,
    expected_type: LuaType,
    function_name: &str,
) -> Result<bool> {
    state
        .check_optional_type(arg_number, expected_type)
        .map_err(|error| name_argument_error(error, function_name))
}

/// Builds a standard-library argument error with a named function and typed detail.
pub(super) fn argument_error(
    state: &State,
    arg_number: isize,
    function_name: &str,
    detail: crate::error::ArgDetail,
) -> Error {
    state.error(ErrorKind::argument_detail(
        arg_number,
        function_name,
        detail,
    ))
}

/// Converts a numeric argument to an exact i64, reporting its argument name.
pub(super) fn exact_integer_value(
    state: &State,
    number: f64,
    arg_number: isize,
    function_name: &str,
) -> Result<i64> {
    exact_i64(number).ok_or_else(|| {
        state.error(ErrorKind::argument_detail(
            arg_number,
            function_name,
            crate::error::ArgDetail::NoInteger,
        ))
    })
}

/// Checks a required numeric argument and converts it to an exact i64.
pub(super) fn arg_integer(
    state: &mut State,
    arg_number: isize,
    function_name: &str,
) -> Result<i64> {
    check_type_named(state, arg_number, LuaType::Number, function_name)?;
    let number = state.to_number(arg_number)?;
    exact_integer_value(state, number, arg_number, function_name)
}

/// Checks an optional numeric argument and converts it to an exact i64.
pub(super) fn opt_integer(
    state: &mut State,
    arg_number: isize,
    function_name: &str,
) -> Result<Option<i64>> {
    if check_optional_type_named(state, arg_number, LuaType::Number, function_name)? {
        Ok(Some(arg_integer(state, arg_number, function_name)?))
    } else {
        Ok(None)
    }
}

// The callback ABI accepts 255 results. RetCount's 255 byte is an encoding
// sentinel for All, not a 254-result runtime cap; keep count validation here
// independent of the bytecode encoding.
/// Implements both global `unpack` and `table.unpack`. Free of charge by
/// design (see the charge comment below).
///
/// Follows Lua 5.4 for both names: elements are read through `__index`, and
/// when `j` is omitted the length comes from `__len`. Lua 5.2's global
/// `unpack` reads raw.
pub(super) fn unpack_values(state: &mut State) -> Result<u8> {
    check_type_named(state, 1, LuaType::Table, "unpack")?;

    let i = opt_integer(state, 2, "unpack")?.unwrap_or(1);
    // As in 5.4, `__len` runs only when no explicit end is given.
    let j = match opt_integer(state, 3, "unpack")? {
        Some(j) => j,
        None => table::sequence_len(state, 1)?,
    };
    if i <= j {
        table::check_sequence_keys(state, i, j, "unpack")?;
    }

    state.set_top(1)?;
    if i > j {
        state.set_top(0)?;
        return Ok(0);
    }
    let span = j.checked_sub(i).ok_or_else(|| {
        state.error(ErrorKind::TooManyResults {
            limit: crate::error::MAX_RESULTS,
        })
    })?;
    if span >= i64::from(crate::error::MAX_RESULTS) {
        return Err(state.error(ErrorKind::TooManyResults {
            limit: crate::error::MAX_RESULTS,
        }));
    }
    // Unpack charges nothing, however many elements it visits: a deliberate
    // game-design choice of the cost model (the budget is not a CPU meter),
    // like `select`. Do not add a per-element charge.
    for index in i..=j {
        state.push_number(index as f64)?;
        state.get_table(1)?;
    }
    state.remove(1)?;
    // span is nonnegative and below MAX_RESULTS here, so the conversion cannot fail.
    let result_count = usize::try_from(span + 1).map_err(|_| {
        state.error(ErrorKind::TooManyResults {
            limit: crate::error::MAX_RESULTS,
        })
    })?;
    crate::error::results_count(result_count).map_err(|kind| state.error(kind))
}
