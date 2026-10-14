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

// Recursive parser entry points share this budget. Visible nesting costs vary
// by construct, so this is a recursion budget rather than a source-level depth.
/// Maximum active recursive parser entries, rather than source nesting levels.
pub const MAX_SYNTAX_DEPTH: u32 = 200;

// Bound compile-time scanning and line-index storage. This build-time policy
// cap also keeps lexer token lengths and one-based line numbers within u32.
/// Maximum source bytes accepted by one compilation.
pub const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;

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
/// Maximum argument count accepted by the runtime call ABI.
pub const MAX_ARGUMENTS: u8 = u8::MAX;
/// Maximum number of values a Rust callback can return.
pub const MAX_RESULTS: u8 = u8::MAX;
