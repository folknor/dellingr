use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use super::super::ast::{LocalDecl, LocalId};
use super::{Bytecode, LiteralPoolIndices, LoopContext, UpvalueDesc};

/// All state scoped to one function. Entering a function constructs this unit;
/// leaving it removes the unit, including its loops.
#[derive(Debug)]
pub(super) struct FuncState<'a> {
    pub(super) chunk: Bytecode,
    pub(super) literal_indices: LiteralPoolIndices,
    /// Names borrow the source; slot order is declaration order.
    pub(super) locals: Vec<&'a str>,
    /// The declaration of each active local, parallel to `locals`.
    pub(super) local_ids: Vec<LocalId>,
    /// Every local declared in this function, by declaration order.
    pub(super) decls: Vec<LocalDecl>,
    pub(super) upvalues: Vec<(&'a str, UpvalueDesc)>,
    /// A nested function must never see an enclosing function's loops.
    pub(super) loops: Vec<LoopContext>,
    /// Local count at each block entry, excluding the function's base scope,
    /// paired with the `captures` length at that entry.
    pub(super) block_starts: Vec<(usize, usize)>,
    /// Append-only log of this function's local slots captured by a nested
    /// function, one entry per first capture by each nested function. A scope
    /// whose log suffix holds no slot at or above its close boundary cannot
    /// have an open upvalue there, so it closes nothing. Never deduplicate by
    /// slot: a later local reusing a slot needs its own entry.
    pub(super) captures: Vec<u8>,
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
            local_ids: Vec::new(),
            decls: Vec::new(),
            upvalues: Vec::new(),
            loops: Vec::new(),
            block_starts: Vec::new(),
            captures: Vec::new(),
        }
    }
}

/// The function being parsed and every function enclosing it. The current
/// frame is a field, not the top of a vector, so it exists by construction:
/// the parser starts with the real root frame, and dereferencing borrows
/// only the function state, leaving the parser's lexer and line stamp
/// independently accessible.
#[derive(Debug)]
pub(super) struct FunctionStack<'a> {
    current: FuncState<'a>,
    /// Enclosing functions, outermost (the root) first.
    enclosing: Vec<FuncState<'a>>,
}

impl<'a> FunctionStack<'a> {
    pub(super) fn new(root: FuncState<'a>) -> Self {
        Self {
            current: root,
            enclosing: Vec::new(),
        }
    }

    /// Active functions, the current one included.
    pub(super) fn depth(&self) -> usize {
        self.enclosing.len() + 1
    }

    /// Level of the current function; the root is level 0.
    pub(super) fn current_level(&self) -> usize {
        self.enclosing.len()
    }

    /// The function at `level`, the root being level 0. Like slice
    /// indexing, a level past the current function panics; callers derive
    /// levels from [`Self::current_level`] downward.
    pub(super) fn frame(&self, level: usize) -> &FuncState<'a> {
        if level == self.enclosing.len() {
            &self.current
        } else {
            &self.enclosing[level]
        }
    }

    pub(super) fn frame_mut(&mut self, level: usize) -> &mut FuncState<'a> {
        if level == self.enclosing.len() {
            &mut self.current
        } else {
            &mut self.enclosing[level]
        }
    }

    /// Make `child` the current function, its parent enclosing it.
    pub(super) fn enter(&mut self, child: FuncState<'a>) {
        let parent = std::mem::replace(&mut self.current, child);
        self.enclosing.push(parent);
    }

    /// Finish the current nested function, restoring its parent. `None` at
    /// the root, which only [`Self::take_root`] finishes.
    pub(super) fn leave(&mut self) -> Option<FuncState<'a>> {
        let parent = self.enclosing.pop()?;
        Some(std::mem::replace(&mut self.current, parent))
    }

    /// Finish the root function. `None` while a nested function is open. The
    /// frame left behind is inert: the parser is consumed right after, so it
    /// never takes part in name lookup, the nesting limit or any read.
    pub(super) fn take_root(&mut self) -> Option<FuncState<'a>> {
        if !self.enclosing.is_empty() {
            return None;
        }
        Some(std::mem::replace(
            &mut self.current,
            FuncState::new(None, 0, false),
        ))
    }
}

impl<'a> Deref for FunctionStack<'a> {
    type Target = FuncState<'a>;

    fn deref(&self) -> &Self::Target {
        &self.current
    }
}

impl DerefMut for FunctionStack<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.current
    }
}
