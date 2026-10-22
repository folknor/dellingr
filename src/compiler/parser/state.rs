use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use super::{Bytecode, LiteralPoolIndices, LoopContext, UpvalueDesc};

/// All state scoped to one function. Entering a function constructs this unit;
/// leaving it removes the unit, including its loops and folding barrier.
#[derive(Debug)]
pub(super) struct FuncState<'a> {
    pub(super) chunk: Bytecode,
    pub(super) literal_indices: LiteralPoolIndices,
    /// Names borrow the source; slot order is declaration order.
    pub(super) locals: Vec<&'a str>,
    pub(super) upvalues: Vec<(&'a str, UpvalueDesc)>,
    /// A nested function must never see an enclosing function's break targets.
    pub(super) loop_breaks: Vec<LoopContext>,
    /// Local count at each block entry, excluding the function's base scope,
    /// paired with the `captures` length at that entry.
    pub(super) block_starts: Vec<(usize, usize)>,
    /// Append-only log of this function's local slots captured by a nested
    /// function, one entry per first capture by each nested function. A scope
    /// whose log suffix holds no slot at or above its close boundary cannot
    /// have an open upvalue there, so its `CloseUpvalues` is omitted. Never
    /// deduplicate by slot: a later local reusing a slot needs its own entry.
    pub(super) captures: Vec<u8>,
    /// Folding cannot merge operands across a patched forward-jump target.
    /// The boundary is exclusive: landing on the first operand is safe, but
    /// landing on the second must still execute that operand independently.
    pub(super) fold_barrier: usize,
}

impl FuncState<'_> {
    pub(super) fn new(source: Option<Arc<[u8]>>, num_params: u8, is_vararg: bool) -> Self {
        Self {
            chunk: Bytecode {
                source,
                num_params,
                is_vararg,
                ..Default::default()
            },
            literal_indices: LiteralPoolIndices::default(),
            locals: Vec::new(),
            upvalues: Vec::new(),
            loop_breaks: Vec::new(),
            block_starts: Vec::new(),
            captures: Vec::new(),
            fold_barrier: 0,
        }
    }
}

/// One uniform stack for both enclosing and current functions. Dereferencing
/// selects the last frame in one place and borrows only the function state,
/// leaving the parser's lexer and line stamp independently accessible.
#[derive(Debug, Default)]
pub(super) struct FunctionStack<'a> {
    pub(super) frames: Vec<FuncState<'a>>,
}

// Deref cannot return a parser error. These two guards enforce the private
// lifecycle boundary: parse_chunk installs a frame before any field access,
// and removes it only after the final access. Recoverable checks inside that
// lifecycle return InternalError instead of relying on this guard.
impl<'a> Deref for FunctionStack<'a> {
    type Target = FuncState<'a>;

    #[allow(clippy::expect_used)] // A function frame is active throughout parsing.
    fn deref(&self) -> &Self::Target {
        self.frames
            .last()
            .expect("function state is accessed only while parsing a chunk")
    }
}

impl DerefMut for FunctionStack<'_> {
    #[allow(clippy::expect_used)] // A function frame is active throughout parsing.
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.frames
            .last_mut()
            .expect("function state is accessed only while parsing a chunk")
    }
}
