//! Shared compiler limits derived from bytecode operand widths.

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
