//! Lua's standard library.
//!
//! Version-sensitive compatibility decisions are per function: `string.format`
//! follows Lua 5.4, `math.random` follows Lua 5.4's interval and error behavior
//! except that `math.random(0)` is rejected as an empty interval,
//! `math.log(x, base)` follows Lua 5.2's general-base calculation, and
//! `math.atan(y, x)` uses the Lua 5.3+ two-argument form. Global `unpack` is kept
//! from Lua 5.2; it and `table.unpack` share one implementation with Lua 5.4
//! semantics, but are two functions (unequal, with their own save ids), where
//! Lua 5.2's are one. The table library (`insert`, `remove`, `concat`,
//! `unpack`, `move`, `sort`) follows Lua 5.4's metamethod rules: lengths come from
//! `__len`, reads go through `__index` and writes through `__newindex`, where
//! Lua 5.2 reads raw. Sort handler calls have a different order from Lua 5.4.
//! The `%p` conversion in
//! `string.format` follows Lua 5.4 and is absent in Lua 5.2.
//! `string.gsub` and `string.gmatch` follow Lua 5.4's empty-match rule: an
//! empty match ending where the previous match ended is skipped.
//! `string.gmatch` also accepts Lua 5.4's optional `init` argument.
//!
//! # Position arguments
//!
//! The library's sequence positions and replacement counts are read by
//! `arg_integer` / `opt_integer`: a number (numeral strings are rejected,
//! see `docs/compatibility.md`) with an exact `i64` value, else an argument
//! error, as in Lua 5.4 (Lua 5.2 truncates `1.5`). An optional argument that is
//! absent or nil takes its default; a required one rejects both. The rules that
//! follow differ per function on purpose and follow Lua 5.4 unless noted; they
//! are not shared code. `len` is the string length, or the sequence length
//! (through `__len` where the function honours it; its result may be a number
//! or a numeral string with an exact `i64` value, negative included).
//!
//! - `string.sub(s, i [, j])`: `j` defaults to -1. Negative counts back from
//!   the end (-1 is the last byte), then `i` clamps up to 1 and `j` down to
//!   `len`; when the resolved `i` exceeds the resolved `j` or `len` (as with
//!   `j = 0`) the result is "". Positions outside the string do not raise.
//! - `string.find`, `match`, `gmatch` `init`: defaults to 1. Negative counts
//!   back from the end; 0 and anything below `-len` mean 1. Past `len + 1`,
//!   `find` and `match` return nil without matching and `gmatch` yields
//!   nothing; at `len + 1` only an empty match can succeed. Lua 5.2's `gmatch`
//!   has no `init`.
//! - `string.gsub` `n` is a count: absent or nil is unlimited, 0 or negative
//!   replaces nothing (Lua 5.2 treats negative as unlimited).
//! - `select(n, ...)`: `"#"` returns the number of arguments after it. A
//!   positive `n` past that count returns nothing; negative counts back from
//!   the last argument; 0 and anything below `-count` raise "index out of
//!   range". The returned suffix is subject to the 255-result limit.
//! - `table.insert(t, pos, v)` and `table.remove(t [, pos])` take `pos`
//!   literally. For a nonnegative `len`: insert needs `1 <= pos <= len + 1`
//!   (the two-argument form appends at `len + 1`); remove defaults to `len` and
//!   accepts `len` or `1..=len + 1`, so 0 and 1 on an empty table. Anything else
//!   is "position out of bounds". For a negative `__len` the metamethod paths
//!   keep 5.4's wrapping checks, which admit negative positions: an explicit
//!   insert `pos` needs `u64(pos.wrapping_sub(1)) < u64(len.wrapping_add(1))`
//!   (the two-argument form uses `len.wrapping_add(1)` itself), remove needs
//!   `pos == len` or `u64(pos.wrapping_sub(1)) <= u64(len)`, where `u64(x)`
//!   reinterprets the bits as unsigned.
//! - `table.concat(t [, sep [, i [, j]]])` and `unpack` / `table.unpack(t [, i
//!   [, j]])`: `i` defaults to 1, `j` to `len` (`concat` evaluates `len` even
//!   when `j` is given, `unpack` only when `j` is absent or nil). Taken literally: no
//!   clamping, no negative counting, `i > j` is empty. `unpack` is subject to
//!   the 255-result limit.
//! - `table.move(a1, f, e, t [, a2])`: `f`, `e` and `t` are required and
//!   literal. `f > e` moves nothing and still returns the destination;
//!   otherwise 5.4's overflow guards apply.
//!
//! `concat`, `unpack`, `move` (source and destination) and the metamethod
//! paths of `insert` and `remove` also refuse a nonempty key range that f64
//! keys cannot represent: a single key must be exact, and a range of several
//! must lie within the inclusive +/-2^53, where consecutive integer keys stop
//! being distinct.

#![deny(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

mod basic;
mod math;
mod string;
mod string_format;
mod table;

pub(crate) use basic::open_base;
#[cfg(feature = "snapshot")]
pub(crate) use string::gmatch_iterator_bytecode;

use crate::LuaType;
use crate::Result;
use crate::State;
use crate::error::{Error, ErrorKind};
use crate::numeral::exact_i64;
use crate::vm::RustFnId;

/// Capacity reservations for the standard-library setup path.
///
/// Math includes its `pi` and `huge` constants. The setup preflight reserves
/// four slots before any library has been installed.
const MATH_LIBRARY_CAPACITY: usize = math::MATH_FUNCTIONS.len() + math::MATH_CONSTANTS.len();
const STRING_LIBRARY_CAPACITY: usize = string::STRING_FUNCTIONS.len();
const TABLE_LIBRARY_CAPACITY: usize = table::TABLE_FUNCTIONS.len();

// `Builtin::COUNT` covers all standard globals except `error`.
pub(crate) const STDLIB_NON_BUILTIN_GLOBALS: usize = basic::NON_BUILTIN_GLOBALS.len();

// `_G`'s metatable, the math, string and table library tables, and the
// string metatable. `_G` itself is the global table, which exists already.
pub(crate) const STDLIB_HEAP_TABLES: usize = 5;

// Function and constant keys in the library tables, the non-builtin global
// names, plus the `_G` protection key. Keep the count next to the lists that
// define its inputs.
pub(crate) const STDLIB_INTERNED_TABLE_KEYS: usize = MATH_LIBRARY_CAPACITY
    + STRING_LIBRARY_CAPACITY
    + TABLE_LIBRARY_CAPACITY
    + STDLIB_NON_BUILTIN_GLOBALS
    + basic::GLOBAL_ENV_PROTECTION_FIELDS.len();

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
    // A host that registered an id the standard library uses (`"print"`,
    // `"string.find"`, `"string.gmatch.iter"`, ...) on a raw State would
    // otherwise end up with one id naming two functions. The host can still
    // use the State after that error (`State::raw` followed by
    // `register_rust_fn` and `open_libs` is the supported route), so the
    // refusal comes before anything is bound or installed. Reopening binds
    // every catalog id to its own entry again, which is no conflict.
    #[cfg(feature = "snapshot")]
    {
        if let Some(id) = state.rust_fns.catalog_conflict() {
            return Err(Error::without_location(
                ErrorKind::DuplicateFunctionRegistration { id },
            ));
        }
        state.rust_fns.bind_catalog();
    }
    state.reserve_stdlib_capacity();
    open_base(state)?;
    math::open_math(state)?;
    string::open_string(state)?;
    table::open_table(state)
}

/// The standard library's Rust function catalog: every library function, and
/// every callback a library function returns rather than installs, at a fixed
/// position. Every State's Rust function table starts with these entries in
/// this order (`RustFnTable::new`), so a library value is the same handle in
/// every State and every build, and the VM recognises `next` and the `ipairs`
/// iterator by handle. Installation order is each `open_*` function's own,
/// unchanged by the catalog order.
struct CatalogSegment {
    /// The library table whose name prefixes each stable save id, or `None`
    /// for ids that are complete names.
    #[cfg(feature = "snapshot")]
    prefix: Option<&'static str>,
    functions: &'static [(&'static str, crate::RustFunc)],
}

/// Callbacks library functions return: the `ipairs` iterator and the two
/// `string.gmatch` iterators, which a saved gmatch closure captures.
const RETURNED_CALLBACKS: &[(&str, crate::RustFunc)] = &[
    ("ipairs.iter", basic::base_ipairs_iter),
    (crate::gmatch_schema::ITERATOR_ID, string::gmatch_iter),
    (
        crate::gmatch_schema::EMPTY_ITERATOR_ID,
        string::gmatch_empty_iter,
    ),
];

const CATALOG: &[CatalogSegment] = &[
    CatalogSegment {
        #[cfg(feature = "snapshot")]
        prefix: None,
        functions: RETURNED_CALLBACKS,
    },
    CatalogSegment {
        #[cfg(feature = "snapshot")]
        prefix: None,
        functions: basic::BASE_FUNCTIONS,
    },
    CatalogSegment {
        #[cfg(feature = "snapshot")]
        prefix: Some("math"),
        functions: math::MATH_FUNCTIONS,
    },
    CatalogSegment {
        #[cfg(feature = "snapshot")]
        prefix: Some("string"),
        functions: string::STRING_FUNCTIONS,
    },
    CatalogSegment {
        #[cfg(feature = "snapshot")]
        prefix: Some("table"),
        functions: table::TABLE_FUNCTIONS,
    },
];

const BASE_SEGMENT: usize = 1;
const MATH_SEGMENT: usize = 2;
const STRING_SEGMENT: usize = 3;
const TABLE_SEGMENT: usize = 4;

/// The catalog position of a segment's first entry.
const fn segment_offset(segment: usize) -> usize {
    let mut offset = 0;
    let mut index = 0;
    while index < segment {
        offset += CATALOG[index].functions.len();
        index += 1;
    }
    offset
}

/// The position of `name` in a segment, found at compile time.
const fn position_in(functions: &[(&str, crate::RustFunc)], name: &str) -> usize {
    let mut index = 0;
    while index < functions.len() {
        let candidate = functions[index].0.as_bytes();
        let wanted = name.as_bytes();
        if candidate.len() == wanted.len() {
            let mut byte = 0;
            while byte < wanted.len() && candidate[byte] == wanted[byte] {
                byte += 1;
            }
            if byte == wanted.len() {
                return index;
            }
        }
        index += 1;
    }
    panic!("the catalog lists every recognised library function")
}

/// The `ipairs` iterator `ipairs` returns.
pub(crate) const IPAIRS_ITER: RustFnId = RustFnId::catalog(0);
/// The general `string.gmatch` iterator.
const GMATCH_ITER: RustFnId = RustFnId::catalog(1);
/// The `string.gmatch` iterator for an empty pattern.
const GMATCH_EMPTY_ITER: RustFnId = RustFnId::catalog(2);
/// Global `next`, which `pairs` also returns.
pub(crate) const NEXT: RustFnId =
    RustFnId::catalog(segment_offset(BASE_SEGMENT) + position_in(basic::BASE_FUNCTIONS, "next"));

/// The handle of a segment's `index`th function.
fn library_fn(segment: usize, index: usize) -> RustFnId {
    debug_assert!(index < CATALOG[segment].functions.len());
    RustFnId::catalog(segment_offset(segment) + index)
}

/// Every catalog function, in catalog order.
pub(crate) fn catalog_functions() -> impl Iterator<Item = crate::RustFunc> {
    CATALOG
        .iter()
        .flat_map(|segment| segment.functions.iter().map(|&(_, function)| function))
}

/// Every catalog entry with its stable save id (`math.sin`, `print`,
/// `ipairs.iter`, ...).
#[cfg(feature = "snapshot")]
pub(crate) fn catalog_ids() -> impl Iterator<Item = (RustFnId, String)> {
    CATALOG
        .iter()
        .flat_map(|segment| {
            segment
                .functions
                .iter()
                .map(move |&(name, _)| match segment.prefix {
                    Some(prefix) => format!("{prefix}.{name}"),
                    None => name.to_owned(),
                })
        })
        .enumerate()
        .map(|(index, id)| (RustFnId::catalog(index), id))
}

/// Installs each function of a library segment into the table on top of the
/// stack, in the segment's order.
fn install_library_functions(state: &mut State, segment: usize) -> Result<()> {
    for (index, &(name, _)) in CATALOG[segment].functions.iter().enumerate() {
        state.set_table_str_key_rust_fn(-1, name, library_fn(segment, index))?;
    }
    Ok(())
}

/// Adds the known standard-library function name to a generic argument error.
///
/// Explicit library names override global aliases and snapshot registration ids.
fn name_argument_error(error: Error, function_name: &str) -> Error {
    error.with_argument_function_name(function_name)
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
        None => crate::native_call!(
            "unpack_values->sequence_len",
            table::sequence_len(state, 1, crate::error::LibraryFunction::Unpack)
        )?,
    };
    if i <= j {
        table::check_sequence_keys(state, i, j, crate::error::LibraryFunction::Unpack)?;
    }

    state.set_top(1)?;
    if i > j {
        state.set_top(0)?;
        return Ok(0);
    }
    let span = j.checked_sub(i).ok_or_else(|| {
        state.error(ErrorKind::TooManyResults {
            limit: crate::limits::MAX_RESULTS,
        })
    })?;
    if span >= i64::from(crate::limits::MAX_RESULTS) {
        return Err(state.error(ErrorKind::TooManyResults {
            limit: crate::limits::MAX_RESULTS,
        }));
    }
    // Unpack charges nothing, however many elements it visits: a deliberate
    // game-design choice of the cost model (the budget is not a CPU meter),
    // like `select`. Do not add a per-element charge.
    for index in i..=j {
        state.push_number(index as f64)?;
        crate::native_call!(
            "unpack_values->get_table_unguarded",
            state.get_table_unguarded(1)
        )?;
    }
    state.remove(1)?;
    // span is nonnegative and below MAX_RESULTS here, so the conversion cannot fail.
    let result_count = usize::try_from(span + 1).map_err(|_| {
        state.error(ErrorKind::TooManyResults {
            limit: crate::limits::MAX_RESULTS,
        })
    })?;
    crate::error::results_count(result_count).map_err(|kind| state.error(kind))
}
