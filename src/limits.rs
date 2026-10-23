//! Fixed resource, encoding, and reference-pinned library limits.
//!
//! These are build-wide constants, not per-State settings. Native stack space
//! needed to reach recursion limits depends on the target and build profile;
//! docs/limits.md, "Native stack", states the envelope they are sized for.
//!
//! No limit consults the native stack itself. A remaining-stack probe (as
//! `stacker`/`psm` offer) would make the depth at which a script fails
//! depend on the thread, the build profile and inlining, which breaks replay
//! determinism, and growing the stack on demand would add a C-toolchain
//! build dependency to a pure-Rust crate. A per-State native-stack setting
//! would likewise make identical scripts behave differently per embedder.
//!
//! Fixed limits have no per-State injection point. Scripts must behave
//! identically for every embedder and across save/restore, so changing a
//! script-visible limit changes the replay and snapshot contract. The snapshot
//! format and verifier validate against these values directly. Internal
//! batching, cache, GC-default, and allocation-capacity tuning lives in the
//! crate-private `tuning` module. The existing public paths for its cost-batch
//! interval and pattern-cache capacities remain as forwarding constants below.

use crate::Result;
use crate::error::{Error, ErrorKind};

pub(crate) mod tuning;

/// Static cost units batched before checking the cost budget.
pub const COST_CHECK_INTERVAL: u64 = tuning::COST_CHECK_INTERVAL;

/// Maximum cached compiled string patterns per State.
pub const PATTERN_CACHE_ENTRIES: usize = tuning::PATTERN_CACHE_ENTRIES;

/// Maximum total source-pattern bytes in the compiled-pattern cache per State.
pub const PATTERN_CACHE_BYTES: usize = tuning::PATTERN_CACHE_BYTES;

// Recursive parser entry points share this budget. Visible nesting costs vary
// by construct, so this is a recursion budget rather than a source-level depth.
// The statement, `if`, expression and unary parsers each charge one unit while
// active; a call or index suffix whose contents recurse also charges
// SYNTAX_SUFFIX_COST. docs/limits.md tabulates the per-construct totals.
/// Syntax budget, in units charged by active recursive parsers, rather than a
/// count of source nesting levels.
pub const MAX_SYNTAX_DEPTH: u32 = 200;

// The suffix loop and suffix parser stay live while their contents recurse,
// and in debug builds those frames are large enough that a one-unit charge let
// nested calls overflow a default 2MB test thread before the budget ran out.
// With the expression and unary units, one nested call or index level costs
// 2 + SYNTAX_SUFFIX_COST. Changing this value changes which sources compile.
/// Additional syntax-budget units charged while the contents of a call
/// argument list, index, table-call or method-call suffix are parsed. This is
/// a parse-time budget, unrelated to runtime instruction cost.
pub const SYNTAX_SUFFIX_COST: u32 = 2;

// Bound compile-time scanning and line-index storage. This build-time policy
// cap also keeps lexer token lengths and one-based line numbers within u32.
/// Maximum source bytes accepted by one compilation. Matches the Lua string
/// limit so `load` can compile any otherwise valid VM string.
pub const MAX_SOURCE_BYTES: usize = MAX_STRING_BYTES;

// Token lengths and the EOF line (at most source bytes + 1) must fit u32.
const _: () = assert!(MAX_SOURCE_BYTES < u32::MAX as usize);

/// Maximum UTF-8 source-name bytes attached to a compiled chunk.
pub const MAX_SOURCE_NAME_BYTES: usize = 256 * 1024;
/// Maximum UTF-8 bytes retained in a diagnostic function name.
pub const MAX_CHUNK_NAME_BYTES: usize = 4 * 1024;
/// Maximum UTF-8 bytes in a registered snapshot function identifier.
pub const MAX_SAVE_IDENTIFIER_BYTES: usize = 4 * 1024;
/// Maximum engine-version bytes in a snapshot.
pub const MAX_ENGINE_VERSION_BYTES: usize = 128;
/// Maximum external-environment token bytes in a snapshot.
pub const MAX_ENVIRONMENT_TOKEN_BYTES: usize = 256;

// Chunk-tree depth includes the root, as it does in save validation. The
// parser checks this before descending into a child function. Because it
// shares MAX_SYNTAX_DEPTH's numeric budget, a chain of nested functions hits
// this guard at the same boundary where the syntax-depth budget would reject
// the next descent; both raise TooManySyntaxLevels. Keep the cap explicit:
// save validation checks chunk depth as its own dimension.
/// Maximum chunk-tree depth, including the root.
pub const MAX_CHUNK_NESTING: usize = MAX_SYNTAX_DEPTH as usize;

// Slot limits are counts: a limit of n permits indices 0 through n - 1.
/// Maximum local slots in a function.
pub const MAX_LOCALS: usize = u8::MAX as usize;
/// Maximum upvalues in a function.
pub const MAX_UPVALUES: usize = u8::MAX as usize;
// Runtime call counts are u8-wide. The fixed bytecode argument cap leaves one
// value for a `__call` receiver. The fixed-result cap remains one below the
// runtime ceiling to preserve the established accepted-program set. The only
// open-count operand is instr::OPEN_COUNT (u16::MAX); the u8 maximum is an
// ordinary fixed count.
/// Maximum fixed arguments in a source call, leaving room for a `__call`
/// receiver.
pub const MAX_FIXED_ARGS: usize = MAX_ARGUMENTS as usize - 1;
/// Maximum fixed results requested by a call or assignment.
pub const MAX_FIXED_RESULTS: usize = MAX_RESULTS as usize - 1;
/// Maximum expressions in one source construct.
pub const MAX_EXPRESSIONS: u8 = u8::MAX;
/// Maximum targets in one assignment.
pub const MAX_ASSIGNMENT_TARGETS: usize = MAX_EXPRESSIONS as usize;
/// Maximum registers reserved by one compiled function.
pub const MAX_REGISTERS: usize = u16::MAX as usize - 1;
/// Maximum assignment receiver offset the compiler accepts.
pub const MAX_ASSIGNMENT_OFFSET: usize = u8::MAX as usize;
/// Maximum direct child functions in a chunk.
pub const MAX_NESTED: usize = u8::MAX as usize;
/// Maximum distinct literals in each numeric or string pool.
pub const MAX_LITERALS: usize = u16::MAX as usize + 1;
/// Maximum inline table templates in a chunk.
pub const MAX_TABLE_TEMPLATES: usize = u8::MAX as usize;
/// Maximum fields in a single inline table template.
pub const MAX_TABLE_TEMPLATE_FIELDS: usize = u8::MAX as usize;
/// Maximum array entries emitted in a table-constructor batch.
pub const MAX_TABLE_BATCH_SIZE: u8 = u8::MAX;
/// Maximum frame slots permitted by the compiler and bytecode verifier.
pub const MAX_FRAME_SLOTS: usize = u8::MAX as usize;

/// Maximum size in bytes of any Lua string.
pub const MAX_STRING_BYTES: usize = 16 * 1024 * 1024;

/// Reject a string whose byte length exceeds the fixed VM limit.
pub(crate) fn check_string_size(size: usize) -> Result<()> {
    if size > MAX_STRING_BYTES {
        return Err(Error::without_location(ErrorKind::StringSizeExceeded {
            size,
            limit: MAX_STRING_BYTES,
        }));
    }
    Ok(())
}

/// Add to a string length without allowing overflow or exceeding its limit.
pub(crate) fn checked_string_growth(current: usize, additional: usize) -> Result<usize> {
    // Saturating rather than wrapping: an overflowing total is reported as
    // usize::MAX, which is above the cap and so rejected instead of wrapping
    // to a small value that would pass the check.
    let size = current.saturating_add(additional);
    check_string_size(size)?;
    Ok(size)
}

/// Maximum simultaneously active Lua function calls. Lua-to-Lua calls run in
/// one dispatch loop with heap-held frames, so reaching this limit needs no
/// native stack per level.
pub const MAX_CALL_DEPTH: u32 = 1000;
/// Maximum simultaneously active calls made from Rust: host `call` and
/// `call_anchor`, metamethod handlers (`__index`, `__newindex`, `__len`,
/// `__tostring`), generic-for iterator functions, and library callbacks such
/// as `table.sort` comparators and `string.gsub` replacement functions. Each
/// such call runs on the native stack, so this is the limit
/// that bounds native recursion; the counterpart of reference Lua's
/// `LUAI_MAXCCALLS`. See docs/limits.md, "Native stack".
// Sized so that every limit at once fits the native-stack envelope in
// docs/limits.md, which `tests/native_stack.rs` drives. It was set when that
// envelope was 2 MiB for unoptimized builds too, where 200, reference Lua's
// value, did not fit. Raising it changes which scripts run, so it is a
// replay-visible change.
pub const MAX_NATIVE_CALL_DEPTH: u32 = 150;
/// Maximum values on the shared Lua/Rust stack, not a host-memory quota.
pub const MAX_STACK_SIZE: usize = 1_000_000;
/// Maximum active steps in an index, newindex, or non-function call chain.
pub const MAX_METAMETHOD_DEPTH: u32 = 200;
/// Maximum argument count accepted by the runtime call ABI. This is a runtime
/// cap on every call, not only on Rust callbacks: a dynamic call such as
/// `f(table.unpack(t))` or `f(...)` with more than this many values raises
/// `TooManyArguments` in pure Lua code. The count is a `u8` because
/// `ArgCount::Fixed` and `RetCount::Fixed` carry `u8`, and `RustFunc` returns
/// `u8`; widening it would change those public APIs, so the cap is deliberate.
pub const MAX_ARGUMENTS: u8 = u8::MAX;
/// Maximum number of values a call can return. Applies to Rust callbacks and
/// equally to Lua functions: `return table.unpack(t)` or `return ...` with more
/// than this many values raises `TooManyResults`. See [`MAX_ARGUMENTS`] for why
/// the width is fixed.
pub const MAX_RESULTS: u8 = u8::MAX;

// Reference-pinned library limits. Keep these values and their error paths
// aligned with the selected Lua library behavior.
/// Reference Lua pattern matcher's maximum recursive call count.
///
/// Reaching this active depth raises the existing `pattern too complex`
/// pattern error. This is Lua's `MAXCCALLS` guard, not a VM call-depth limit.
pub const MAXCCALLS: usize = 200;

/// Maximum captures accepted by the Lua pattern compiler, including position
/// captures such as `()`.
///
/// The next capture raises the existing `too many captures` pattern error.
pub const LUA_MAXCAPTURES: usize = 32;

/// Maximum bytes in one `string.format` directive, matching Lua 5.4's
/// `lstrlib` `MAX_FORMAT - 10` bound.
pub const MAX_FORMAT_LEN: usize = 22;

/// Maximum decimal digits accepted in a `string.format` width or precision.
pub const MAX_FORMAT_FIELD_DIGITS: usize = 2;

/// Smallest base accepted by `tonumber(value, base)`.
pub const TONUMBER_BASE_MIN: i64 = 2;

/// Largest base accepted by `tonumber(value, base)`.
pub const TONUMBER_BASE_MAX: i64 = 36;

/// Exclusive upper bound on the length `table.sort` accepts, matching Lua
/// 5.4's `INT_MAX` check. A length at or above it raises `array too big`.
pub const TABLE_SORT_MAX_LEN: i64 = i32::MAX as i64;
