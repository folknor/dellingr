//! Functions and types associated with converting source code into bytecode.

mod exp_desc;
mod lexer;
mod parser;
mod token;

use super::Instr;
use super::Result;
use super::error;

/// Describes where an upvalue comes from when creating a closure.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum UpvalueDesc {
    /// Capture a local variable from the immediately enclosing function.
    Local(u8),
    /// Capture an upvalue from the immediately enclosing function.
    Upvalue(u8),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Chunk {
    pub(super) code: Vec<Instr>,
    pub(super) number_literals: Vec<f64>,
    pub(super) string_literals: Vec<Vec<u8>>,
    pub(super) num_params: u8,
    pub(super) num_locals: u8,
    pub(super) nested: Vec<Chunk>,
    /// Describes the upvalues this function captures.
    pub(super) upvalues: Vec<UpvalueDesc>,
    /// Whether this function accepts varargs (...).
    pub(super) is_vararg: bool,
    /// Optional function name (for debugging/analysis).
    pub(super) name: Option<String>,
    /// Source name (file path or chunk identifier like "[string]").
    pub(super) source: Option<String>,
    /// Maps instruction index to source line number.
    /// line_info[i] is the line number for code[i].
    pub(super) line_info: Vec<u32>,
}

#[hotpath::measure]
pub(super) fn parse_str(source: impl AsRef<str>) -> Result<Chunk> {
    parser::parse_str(source.as_ref())
}

#[hotpath::measure]
pub(super) fn parse_str_named(
    source: impl AsRef<str>,
    source_name: Option<String>,
) -> Result<Chunk> {
    parser::parse_str_named(source.as_ref(), source_name)
}
