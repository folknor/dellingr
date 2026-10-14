//! Shared compiler limits.

// Recursive parser entry points share this budget. Visible nesting costs vary
// by construct, so this is a recursion budget rather than a source-level depth.
pub(crate) const MAX_SYNTAX_DEPTH: u32 = 200;

// Bound compile-time scanning and line-index storage. This build-time policy
// cap also keeps lexer token lengths and one-based line numbers within u32.
pub(crate) const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;

// Each verifier marker stack can grow at most once per parser syntax-budget
// entry. Keep that cap explicit so the verifier does not borrow the parser's
// meaning by importing its constant directly.
pub(crate) const MAX_VERIFIER_MARKER_DEPTH: usize = MAX_SYNTAX_DEPTH as usize;

// Chunk-tree depth includes the root, as it does in save validation. The
// parser checks this before descending into a child function. Because it
// shares MAX_SYNTAX_DEPTH's numeric budget, a chain of nested functions hits
// this guard at the same boundary where the syntax-depth budget would reject
// the next descent; both raise TooManySyntaxLevels. Keep the cap explicit:
// save validation checks chunk depth as its own dimension.
pub(crate) const MAX_CHUNK_NESTING: usize = MAX_SYNTAX_DEPTH as usize;

// These are counts: indices 0 through 254 name 255 values. Operand value 255
// is reserved for dynamic argument and result counts.
pub(crate) const MAX_LOCALS: usize = u8::MAX as usize;
pub(crate) const MAX_UPVALUES: usize = u8::MAX as usize;
pub(crate) const MAX_FIXED_ARGS: usize = u8::MAX as usize - 1;
pub(crate) const MAX_FIXED_RESULTS: usize = MAX_FIXED_ARGS;
pub(crate) const MAX_EXPRESSIONS: u8 = u8::MAX;
pub(crate) const MAX_ASSIGNMENT_OFFSET: usize = u8::MAX as usize;
pub(crate) const MAX_NESTED: usize = u8::MAX as usize;
pub(crate) const MAX_LITERALS: usize = u16::MAX as usize + 1;
pub(crate) const MAX_TABLE_TEMPLATES: usize = u8::MAX as usize;
pub(crate) const MAX_TABLE_TEMPLATE_FIELDS: usize = u8::MAX as usize;
pub(crate) const MAX_TABLE_BATCH_SIZE: u8 = u8::MAX;
pub(crate) const MAX_FRAME_SLOTS: usize = u8::MAX as usize;
