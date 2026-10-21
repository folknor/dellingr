//! Fixed resource and encoding limits.
//!
//! These are build-wide constants, not per-State settings. Native stack space
//! needed to reach recursion limits depends on the target and build profile.
//!
//! They are constants on purpose, with no per-State injection point. Scripts
//! must behave identically for every embedder and across save/restore, so a
//! tunable limit would become part of the replay and snapshot contract; the
//! snapshot format and verifier validate against these values directly. The
//! cost is that tests of the large limits (call depth, stack cap) exercise the
//! real values rather than small ones.

use crate::Result;
use crate::error::{Error, ErrorKind};

// Recursive parser entry points share this budget. Visible nesting costs vary
// by construct, so this is a recursion budget rather than a source-level depth.
/// Maximum active recursive parser entries, rather than source nesting levels.
pub const MAX_SYNTAX_DEPTH: u32 = 200;

// Bound compile-time scanning and line-index storage. This build-time policy
// cap also keeps lexer token lengths and one-based line numbers within u32.
/// Maximum source bytes accepted by one compilation. Matches the Lua string
/// limit so `load` can compile any otherwise valid VM string.
pub const MAX_SOURCE_BYTES: usize = MAX_STRING_BYTES;

// Token lengths and the EOF line (at most source bytes + 1) must fit u32.
const _: () = assert!(MAX_SOURCE_BYTES < u32::MAX as usize);
// Call sites pack lines into 24 bits; the compiler asserts this cap against
// that field's mask where the field is defined.

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

// Each verifier marker stack can grow at most once per parser syntax-budget
// entry. Keep that cap explicit so the verifier does not borrow the parser's
// meaning by importing its constant directly.
/// Maximum entries in each bytecode verifier marker stack.
pub const MAX_VERIFIER_MARKER_DEPTH: usize = MAX_SYNTAX_DEPTH as usize;

// Chunk-tree depth includes the root, as it does in save validation. The
// parser checks this before descending into a child function. Because it
// shares MAX_SYNTAX_DEPTH's numeric budget, a chain of nested functions hits
// this guard at the same boundary where the syntax-depth budget would reject
// the next descent; both raise TooManySyntaxLevels. Keep the cap explicit:
// save validation checks chunk depth as its own dimension.
/// Maximum chunk-tree depth, including the root.
pub const MAX_CHUNK_NESTING: usize = MAX_SYNTAX_DEPTH as usize;

// These are counts: indices 0 through 254 name 255 values. Operand value 255
// is reserved for dynamic argument and result counts.
/// Maximum local slots in a function.
pub const MAX_LOCALS: usize = u8::MAX as usize;
/// Maximum upvalues in a function.
pub const MAX_UPVALUES: usize = u8::MAX as usize;
/// Maximum fixed arguments before the dynamic-count sentinel.
pub const MAX_FIXED_ARGS: usize = u8::MAX as usize - 1;
/// Maximum fixed results before the all-results sentinel.
pub const MAX_FIXED_RESULTS: usize = MAX_FIXED_ARGS;
/// Maximum expressions in one source construct.
pub const MAX_EXPRESSIONS: u8 = u8::MAX;
/// Maximum assignment receiver offset encoded by bytecode.
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
/// Maximum frame slots encoded by bytecode.
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

/// Maximum simultaneously active Lua calls; not a native-stack guarantee.
pub const MAX_CALL_DEPTH: u32 = 1000;
/// Maximum values on the shared Lua/Rust stack, not a host-memory quota.
pub const MAX_STACK_SIZE: usize = 1_000_000;
/// Maximum active steps in an index, newindex, or non-function call chain.
pub const MAX_METAMETHOD_DEPTH: u32 = 200;
/// Maximum cached gmatch patterns per State.
pub const GMATCH_PATTERN_CACHE_ENTRIES: usize = 256;
/// Maximum total source-pattern bytes cached by gmatch per State.
pub const GMATCH_PATTERN_CACHE_BYTES: usize = MAX_STRING_BYTES;
/// Static cost units batched before checking the cost budget.
pub const COST_CHECK_INTERVAL: u64 = 64;
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
