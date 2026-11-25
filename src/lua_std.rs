//! Lua's standard library.
//!
//! Version-sensitive compatibility decisions are per function: `math.random`
//! follows Lua 5.4's interval and error behavior except that `math.random(0)`
//! is rejected as an empty interval,
//! `math.log(x, base)` follows Lua 5.2's general-base calculation, and
//! `math.atan(y, x)` uses the Lua 5.3+ two-argument form. Global `unpack` is kept
//! from Lua 5.2; it and `table.unpack` share one implementation with Lua 5.4
//! semantics, but are two functions (unequal, with their own save ids), where
//! Lua 5.2's are one. The table library (`insert`, `remove`, `concat`,
//! `unpack`, `move`, `sort`) follows Lua 5.4's metamethod rules: lengths come from
//! `__len`, reads go through `__index` and writes through `__newindex`, where
//! Lua 5.2 reads raw. Sort handler calls have a different order from Lua 5.4.
//! The string library's version choices are listed in `docs/compatibility.md`.
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
//! being distinct. The `ipairs` iterator's control is read the same way: an
//! exact integer, else an argument error, and within `-2^53..2^53` (upper
//! bound excluded) so the next key is its exact successor, else the range
//! error.

#![deny(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

mod basic;
pub(crate) mod math;
mod string;
mod string_format;
mod table;

pub(crate) use basic::{ipairs_next_index, open_base};

use crate::LuaType;
use crate::Result;
use crate::State;
use crate::error::{Error, ErrorKind};
use crate::numeral::exact_i64;
use crate::vm::{Leaf, RustFnId};

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
/// The current setup fits this reservation, so a stack-cap refusal happens
/// before any library or catalog mutation. Under `snapshot`, catalog conflicts
/// are also rejected before the catalog is bound. These checks make setup
/// all-or-nothing for those refusal conditions; a later setup error can still
/// leave the catalog bound or some libraries installed because setup is not
/// transactional.
///
/// The per-push checks remain the backstop if a future library ever exceeds
/// this reservation; they would report the overflow correctly, but setup
/// could already be partially installed.
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
                ErrorKind::DuplicateFunctionRegistration { id: id.to_owned() },
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
///
/// Each entry's string is its catalog id, written out in full (`"math.sin"`):
/// the entry's diagnostic name in every build and its stable save id under
/// `snapshot`. Being a compile-time string, it is borrowed by every State's
/// function table rather than copied into it, so building a State allocates
/// no catalog names or ids (`RustFnTable::new`, `RustFnTable::bind_catalog`).
/// The key a library table installs the entry under is the id with the
/// segment's prefix and dot removed (`install_key`). `check_catalog_ids`
/// makes every prefixed id carry its prefix and every id unique at compile
/// time.
struct CatalogSegment {
    /// The library table the segment installs into, whose name and a dot
    /// begin each of its ids, or `None` for ids that are complete names
    /// (the base segment's are also its global names).
    prefix: Option<&'static str>,
    functions: &'static [(&'static str, crate::RustFunc)],
}

/// The catalog id of the `string.gmatch` step. A script never sees the step
/// as a value (it calls an iterator); the id names it in diagnostics and
/// lets a host that looks it up step an iterator it holds.
const GMATCH_STEP_ID: &str = "string.gmatch.iter";

/// Callbacks library functions return or run: the `ipairs` iterator, and the
/// step a `string.gmatch` iterator runs.
const RETURNED_CALLBACKS: &[(&str, crate::RustFunc)] = &[
    ("ipairs.iter", basic::base_ipairs_iter),
    (GMATCH_STEP_ID, string::gmatch_step),
];

const CATALOG: &[CatalogSegment] = &[
    CatalogSegment {
        prefix: None,
        functions: RETURNED_CALLBACKS,
    },
    CatalogSegment {
        prefix: None,
        functions: basic::BASE_FUNCTIONS,
    },
    CatalogSegment {
        prefix: Some("math"),
        functions: math::MATH_FUNCTIONS,
    },
    CatalogSegment {
        prefix: Some("string"),
        functions: string::STRING_FUNCTIONS,
    },
    CatalogSegment {
        prefix: Some("table"),
        functions: table::TABLE_FUNCTIONS,
    },
];

/// How many entries the catalog has: every Rust function id below this is a
/// library function.
pub(crate) const CATALOG_LEN: usize = segment_offset(CATALOG.len());

const BASE_SEGMENT: usize = 1;
const MATH_SEGMENT: usize = 2;
const STRING_SEGMENT: usize = 3;
const TABLE_SEGMENT: usize = 4;

/// The catalog position of a segment's first entry.
const fn segment_offset(segment: usize) -> usize {
    let mut offset = 0;
    let mut index = 0;
    // bounded: segment is an index into the fixed library catalog.
    while index < segment {
        offset += CATALOG[index].functions.len();
        index += 1;
    }
    offset
}

/// Whether `bytes` begins with `prefix`, at compile time.
const fn starts_with(bytes: &[u8], prefix: &[u8]) -> bool {
    if bytes.len() < prefix.len() {
        return false;
    }
    let mut byte = 0;
    // bounded: both are fixed catalog strings.
    while byte < prefix.len() {
        if bytes[byte] != prefix[byte] {
            return false;
        }
        byte += 1;
    }
    true
}

/// Whether two strings are equal, at compile time.
const fn same_str(a: &str, b: &str) -> bool {
    a.len() == b.len() && starts_with(a.as_bytes(), b.as_bytes())
}

/// The position of `name` in a segment, found at compile time.
const fn position_in(functions: &[(&str, crate::RustFunc)], name: &str) -> usize {
    let mut index = 0;
    // bounded: the library function list is fixed at compile time.
    while index < functions.len() {
        if same_str(functions[index].0, name) {
            return index;
        }
        index += 1;
    }
    panic!("the catalog lists every recognised library function")
}

/// Compile-time check of the catalog ids: an id in a prefixed segment is
/// the prefix, a dot and a nonempty name, so `install_key` can slice it, and
/// no two entries share an id, so an id names exactly one entry (a repeated
/// id would otherwise collapse into one `by_id` binding).
const fn check_catalog_ids() {
    let mut segment = 0;
    // bounded: segment is an index into the fixed library catalog.
    while segment < CATALOG.len() {
        let functions = CATALOG[segment].functions;
        let mut index = 0;
        // bounded: each library segment is a fixed catalog list.
        while index < functions.len() {
            let id = functions[index].0;
            if let Some(prefix) = CATALOG[segment].prefix {
                assert!(
                    id.len() > prefix.len() + 1
                        && starts_with(id.as_bytes(), prefix.as_bytes())
                        && id.as_bytes()[prefix.len()] == b'.',
                    "a prefixed catalog id is its prefix, a dot and a name"
                );
            }
            let mut other_segment = segment;
            let mut other = index + 1;
            // bounded: the rest of the fixed library catalog.
            while other_segment < CATALOG.len() {
                let others = CATALOG[other_segment].functions;
                // bounded: each library segment is a fixed catalog list.
                while other < others.len() {
                    assert!(!same_str(id, others[other].0), "catalog ids are unique");
                    other += 1;
                }
                other_segment += 1;
                other = 0;
            }
            index += 1;
        }
        segment += 1;
    }
}

const _: () = check_catalog_ids();

/// The key a segment's entry is installed under in its library table: its
/// id without the segment's prefix and dot (`sin` for `math.sin`).
fn install_key(segment: &CatalogSegment, id: &'static str) -> &'static str {
    match segment.prefix {
        Some(prefix) => &id[prefix.len() + 1..],
        None => id,
    }
}

/// The `ipairs` iterator `ipairs` returns.
pub(crate) const IPAIRS_ITER: RustFnId = RustFnId::catalog(0);
/// The step every `string.gmatch` iterator runs when called (`vm::GmatchIter`).
pub(crate) const GMATCH_STEP: RustFnId = RustFnId::catalog(1);
/// Global `next`, which `pairs` also returns.
pub(crate) const NEXT: RustFnId =
    RustFnId::catalog(segment_offset(BASE_SEGMENT) + position_in(basic::BASE_FUNCTIONS, "next"));

/// The catalog position of the entry with id `id`, found at compile time.
const fn catalog_position(id: &str) -> usize {
    let mut segment = 0;
    // bounded: segment is an index into the fixed library catalog.
    while segment < CATALOG.len() {
        let functions = CATALOG[segment].functions;
        let mut index = 0;
        // bounded: each library segment is a fixed catalog list.
        while index < functions.len() {
            if same_str(functions[index].0, id) {
                return segment_offset(segment) + index;
            }
            index += 1;
        }
        segment += 1;
    }
    panic!("the catalog lists every library leaf")
}

/// The catalog entries the dispatch loop executes directly when Lua calls
/// them (`vm/leaf.rs`), by id.
const LEAF_IDS: &[(&str, Leaf)] = &[
    ("math.floor", Leaf::Floor),
    ("math.ceil", Leaf::Ceil),
    ("math.sqrt", Leaf::Sqrt),
    ("math.abs", Leaf::Abs),
    ("math.deg", Leaf::Deg),
    ("math.rad", Leaf::Rad),
    ("math.sin", Leaf::Sin),
    ("math.cos", Leaf::Cos),
    ("math.tan", Leaf::Tan),
    ("math.asin", Leaf::Asin),
    ("math.acos", Leaf::Acos),
    ("math.exp", Leaf::Exp),
    ("math.atan", Leaf::Atan),
    ("math.log", Leaf::Log),
    ("math.atan2", Leaf::Atan2),
    ("math.fmod", Leaf::Fmod),
    ("math.min", Leaf::Min),
    ("math.max", Leaf::Max),
    ("math.modf", Leaf::Modf),
    ("math.random", Leaf::Random),
    ("type", Leaf::Type),
    ("rawequal", Leaf::RawEqual),
    ("rawlen", Leaf::RawLen),
    ("tonumber", Leaf::ToNumber),
    ("rawget", Leaf::RawGet),
    ("rawset", Leaf::RawSet),
    ("select", Leaf::Select),
    ("table.insert", Leaf::TableInsert),
    ("table.remove", Leaf::TableRemove),
    ("table.pack", Leaf::TablePack),
];

/// Each catalog entry's leaf, if it is one, indexed by catalog position.
const LEAVES: [Option<Leaf>; CATALOG_LEN] = {
    let mut leaves = [None; CATALOG_LEN];
    let mut index = 0;
    // bounded: the fixed leaf list.
    while index < LEAF_IDS.len() {
        let position = catalog_position(LEAF_IDS[index].0);
        assert!(leaves[position].is_none(), "a leaf is listed once");
        leaves[position] = Some(LEAF_IDS[index].1);
        index += 1;
    }
    leaves
};

/// The leaf a Rust function is, if it is a catalog entry the dispatch loop
/// executes directly. Host entries never are, whatever function they call.
#[inline(always)]
pub(crate) fn leaf_of(function: RustFnId) -> Option<Leaf> {
    LEAVES.get(function.position()).copied().flatten()
}

/// The catalog entries whose successful return leaves no unchecked host
/// execution behind, by id: the entry runs no host code itself (it calls no
/// `HostCallbacks` method on the way to `Ok`), and any host Rust callback it
/// reaches through Lua, a metamethod, a comparator or a replacement returns
/// through `invoke_rust_fn`, which checked the floating-point environment
/// there. `on_error` runs only on an escaping error, which every entry
/// propagates. `invoke_rust_fn` skips the return-time environment read for
/// these. An entry missing from the list keeps the read: `print` (it calls
/// `on_print` and can return `Ok` right after) and any entry added to the
/// catalog without this audit. Library leaves are listed too, for their
/// callback fallback.
const HOST_FREE_RETURN_IDS: &[&str] = &[
    "ipairs.iter",
    GMATCH_STEP_ID,
    "ipairs",
    "next",
    "pairs",
    "error",
    "type",
    "tonumber",
    "tostring",
    "unpack",
    "getmetatable",
    "setmetatable",
    "rawget",
    "rawset",
    "rawequal",
    "rawlen",
    "select",
    "math.sin",
    "math.cos",
    "math.atan2",
    "math.sqrt",
    "math.abs",
    "math.min",
    "math.max",
    "math.floor",
    "math.ceil",
    "math.random",
    "math.tan",
    "math.acos",
    "math.asin",
    "math.atan",
    "math.deg",
    "math.rad",
    "math.exp",
    "math.log",
    "math.fmod",
    "math.modf",
    "string.sub",
    "string.find",
    "string.format",
    "string.len",
    "string.upper",
    "string.lower",
    "string.reverse",
    "string.match",
    "string.gmatch",
    "string.gsub",
    "table.insert",
    "table.remove",
    "table.sort",
    "table.unpack",
    "table.pack",
    "table.concat",
    "table.move",
];

/// `HOST_FREE_RETURN_IDS` indexed by catalog position.
const HOST_FREE_RETURNS: [bool; CATALOG_LEN] = {
    let mut host_free = [false; CATALOG_LEN];
    let mut index = 0;
    // bounded: the fixed audited list.
    while index < HOST_FREE_RETURN_IDS.len() {
        let position = catalog_position(HOST_FREE_RETURN_IDS[index]);
        assert!(!host_free[position], "an entry is listed once");
        host_free[position] = true;
        index += 1;
    }
    host_free
};

/// Whether a successful return of `function` leaves no unchecked host
/// execution behind (`HOST_FREE_RETURN_IDS`). Host entries never do.
#[inline(always)]
pub(crate) fn returns_without_host_code(function: RustFnId) -> bool {
    HOST_FREE_RETURNS
        .get(function.position())
        .copied()
        .unwrap_or(false)
}

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

/// Every catalog entry with its id (`math.sin`, `print`, `ipairs.iter`, ...):
/// its diagnostic name in every build, and its stable save id under the
/// `snapshot` feature.
pub(crate) fn catalog_ids() -> impl Iterator<Item = (RustFnId, &'static str)> {
    CATALOG
        .iter()
        .flat_map(|segment| segment.functions.iter().map(|&(id, _)| id))
        .enumerate()
        .map(|(index, id)| (RustFnId::catalog(index), id))
}

/// Installs each function of a library segment into the table on top of the
/// stack, in the segment's order.
fn install_library_functions(state: &mut State, segment: usize) -> Result<()> {
    let segment_entries = &CATALOG[segment];
    // bounded: each library segment is a fixed catalog list.
    for (index, &(id, _)) in segment_entries.functions.iter().enumerate() {
        let key = install_key(segment_entries, id);
        state.set_table_str_key_rust_fn(-1, key, library_fn(segment, index))?;
    }
    Ok(())
}

/// Adds the known standard-library function name to a generic argument error.
///
/// The explicit library name replaces the entry's catalog-id name, keeping
/// each function's established wording.
fn name_argument_error(error: Error, function_name: &str) -> Error {
    error.with_argument_function_name(function_name)
}

/// A library function's own copy of a string or number argument, for a
/// result made by rewriting it in place (`upper`, `lower`, `reverse`) or
/// for bytes kept across calls that can run Lua (`table.concat`'s
/// separator). The storage is chosen once, from the length: up to the
/// pool's inline cap the copy lives in a stack array and is interned by
/// slice, so a short result never allocates; a longer one is a `Vec` the
/// pool adopts. Nothing grows it, so no append pays a storage test.
pub(super) enum BytesCopy {
    Inline {
        len: u8,
        bytes: [u8; crate::vm::INLINE_STRING_BYTES],
    },
    Heap(Vec<u8>),
}

impl BytesCopy {
    pub(super) fn new(source: &[u8]) -> Self {
        let mut bytes = [0u8; crate::vm::INLINE_STRING_BYTES];
        match bytes.get_mut(..source.len()) {
            Some(dest) => {
                dest.copy_from_slice(source);
                let len = u8::try_from(source.len()).expect("the inline cap fits a u8");
                Self::Inline { len, bytes }
            }
            None => Self::Heap(source.to_vec()),
        }
    }

    /// The argument at `idx`, which the caller has type-gated as a string or
    /// a number: a string's bytes, a number's default spelling.
    pub(super) fn of_string_or_number(state: &State, idx: isize) -> Result<Self> {
        if state.typ(idx) == LuaType::Number {
            let text = crate::number_text::number_to_text(state.to_number(idx)?);
            Ok(Self::new(text.as_bytes()))
        } else {
            Ok(Self::new(state.to_bytes(idx)?))
        }
    }

    pub(super) fn as_slice(&self) -> &[u8] {
        match self {
            Self::Inline { len, bytes } => &bytes[..usize::from(*len)],
            Self::Heap(vec) => vec,
        }
    }

    pub(super) fn as_mut_slice(&mut self) -> &mut [u8] {
        match self {
            Self::Inline { len, bytes } => &mut bytes[..usize::from(*len)],
            Self::Heap(vec) => vec,
        }
    }

    /// Push the bytes as a string, with `push_bytes`' refusal order and
    /// collection point either way.
    pub(super) fn push(self, state: &mut State) -> Result<()> {
        match self {
            Self::Inline { len, bytes } => state.push_bytes(&bytes[..usize::from(len)]),
            Self::Heap(vec) => state.push_owned_bytes(vec),
        }
    }
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

/// `check_type_named` for a number followed by `to_number`, in one pass
/// (`State::check_number`). Only for an argument checked and read before the
/// next one is checked: `atan2` and `fmod` check both before reading either.
pub(super) fn check_number_named(
    state: &mut State,
    arg_number: isize,
    function_name: &str,
) -> Result<f64> {
    state
        .check_number(arg_number)
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
    let number = check_number_named(state, arg_number, function_name)?;
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

// The callback ABI accepts MAX_RESULTS (255) results. RetCount encodes All as
// OPEN_COUNT (u16::MAX), leaving 255 as an ordinary fixed count. Validate
// against the runtime ceiling here, not the narrower MAX_FIXED_RESULTS cap
// for fixed call results.
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
    if i > j {
        state.set_top(0)?;
        return Ok(0);
    }
    // Refuse an impossible result count before checking whether every key is
    // exactly representable.
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
    table::check_sequence_keys(state, i, j, crate::error::LibraryFunction::Unpack)?;
    state.set_top(1)?;
    // Unpack charges nothing, however many elements it visits: a deliberate
    // game-design choice of the cost model (the budget is not a CPU meter),
    // like `select`. Do not add a per-element charge.
    // bounded: at most MAX_RESULTS elements, checked above.
    for index in i..=j {
        // A raw hit, or a miss with no `__index`, is pushed straight from the
        // table; either way one slot is checked per element, at the element.
        if !state.push_sequence_raw(1, index)? {
            state.push_number(index as f64)?;
            crate::native_call!(
                "unpack_values->get_table_unguarded",
                state.get_table_unguarded(1)
            )?;
        }
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
