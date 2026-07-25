//! This module holds enums which describe the different types of Lua
//! expressions.

use crate::instr::Builtin;

#[derive(Debug)]
pub(super) enum ExpDesc {
    Prefix(PrefixExp),
    /// A vararg expression (...)
    Vararg,
    Other,
}

/// A call's argument count and opening line, packed into four bytes.
///
/// `PrefixExp` is copied throughout expression parsing. Holding the line as a
/// separate `u32` field beside a `u8` makes that one variant four times the
/// size of every other, so the two are packed: the line occupies the low 24
/// bits and the argument count the high 8. 2^24 lines is far past any real
/// source file, and a longer one saturates rather than reporting a wrong line
/// from wraparound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CallSite(u32);

impl CallSite {
    const LINE_MASK: u32 = (1 << 24) - 1;

    pub(super) fn new(num_args: u8, line: u32) -> Self {
        Self((u32::from(num_args) << 24) | line.min(Self::LINE_MASK))
    }

    pub(super) fn num_args(self) -> u8 {
        (self.0 >> 24) as u8
    }

    pub(super) fn line(self) -> u32 {
        self.0 & Self::LINE_MASK
    }
}

/// A "prefix expression" is an expression which could be followed by certain
/// extensions and still be a valid expression.
///
/// `variant_size_differences` is allowed here deliberately, against the usual
/// "fix the code, don't disable the lint" rule. The whole enum is 8 bytes; the
/// lint exists to catch enums bloated by one large variant, which this is not.
/// Every alternative is worse: boxing costs a heap allocation per prefix
/// expression on the parser's hot path, and narrowing the line to `u16` would
/// saturate traceback lines in generated data scripts past 65k lines - which is
/// precisely the workload the capacity findings are about.
#[allow(variant_size_differences)]
#[derive(Clone, Debug)]
pub(super) enum PrefixExp {
    /// One of the variants of `PlaceExp`
    Place(PlaceExp),
    /// A function call: its argument count and the line the call opens on.
    FunctionCall(CallSite),
    /// An expression wrapped in parentheses
    Parenthesized,
}

/// This represents an expression which can appear on the left-hand side of an assignment.
/// Also called an "lvalue" in other languages.
#[derive(Clone, Debug)]
pub(super) enum PlaceExp {
    /// A local variable, and its index in the list of locals
    Local(u8),
    /// An upvalue (captured variable from enclosing scope), and its index
    Upvalue(u8),
    /// A global variable, and its index in the list of string literals
    Global(u16),
    /// A well-known builtin global (print, pairs, etc.) - uses fast array access
    Builtin(Builtin),
    /// A table index, with `[` and `]`
    TableIndex,
    /// A field access, and the index of the field's identifier in the list of
    /// string literals
    FieldAccess(u16),
}

impl From<PrefixExp> for ExpDesc {
    fn from(exp: PrefixExp) -> Self {
        Self::Prefix(exp)
    }
}

impl From<PlaceExp> for PrefixExp {
    fn from(exp: PlaceExp) -> Self {
        Self::Place(exp)
    }
}
