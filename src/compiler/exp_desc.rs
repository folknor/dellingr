//! This module holds enums which describe the different types of Lua
//! expressions.

use crate::instr::{ArgCount, Builtin, Instr};

#[derive(Debug)]
pub(super) enum ExpDesc {
    /// A final call or vararg expression that can produce multiple values.
    MultiTail(TailKind),
    Other,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum TailKind {
    /// A function call whose instruction result count depends on its tail use.
    FunctionCall(CallSite),
    /// A vararg expression whose instruction result count depends on its tail use.
    Vararg,
}

/// A call's argument count and opening line, packed into four bytes.
/// The accessor boundary uses ArgCount so dynamic calls are decoded rather
/// than disguised as Fixed(255). The packed representation is parser-local.
///
/// `PrefixExp` is copied throughout expression parsing. Holding the line as a
/// separate `u32` field beside a `u8` makes that one variant four times the
/// size of every other, so the two are packed: the line occupies the low 24
/// bits and the argument count the high 8. The saturation is unreachable in
/// practice, so the narrower width versus the `u32` in `line_info` is not a
/// real divergence: `MAX_SOURCE_BYTES` is at most 2^24 bytes (asserted below)
/// and a call needs at least a few bytes, so no call can open on a line at or
/// past 2^24. The `min` is only a guard against wraparound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CallSite(u32);

// Every call opens on a line strictly below the source byte cap, so the cap
// must fit the packed line field.
const _: () = assert!(crate::limits::MAX_SOURCE_BYTES <= CallSite::LINE_MASK as usize + 1);

impl CallSite {
    const LINE_MASK: u32 = (1 << 24) - 1;

    pub(super) fn new(num_args: ArgCount, line: u32) -> Self {
        Self((u32::from(num_args.to_u8()) << 24) | line.min(Self::LINE_MASK))
    }

    pub(super) fn num_args(self) -> ArgCount {
        ArgCount::from_u8(self.0.to_be_bytes()[0])
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
/// saturate traceback lines in generated data scripts past 65k lines, and large
/// generated scripts are a supported workload.
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
        match exp {
            PrefixExp::FunctionCall(call) => Self::MultiTail(TailKind::FunctionCall(call)),
            // No consumer distinguishes a non-call prefix from any other
            // single-value expression.
            PrefixExp::Place(_) | PrefixExp::Parenthesized => Self::Other,
        }
    }
}

impl From<PlaceExp> for PrefixExp {
    fn from(exp: PlaceExp) -> Self {
        Self::Place(exp)
    }
}

impl PlaceExp {
    pub(super) fn get_instr(&self) -> Instr {
        match self {
            Self::Local(index) => Instr::get_local(*index),
            Self::Upvalue(index) => Instr::get_upvalue(*index),
            Self::Global(index) => Instr::get_global(*index, None),
            Self::Builtin(builtin) => Instr::get_builtin(*builtin),
            Self::FieldAccess(index) => Instr::get_field(*index, None),
            Self::TableIndex => Instr::get_table(),
        }
    }

    pub(super) fn set_instr(&self, stack_offset: u8) -> Instr {
        match self {
            Self::Local(index) => Instr::set_local(*index),
            Self::Upvalue(index) => Instr::set_upvalue(*index),
            Self::Global(index) => Instr::set_global(*index, None),
            Self::Builtin(builtin) => Instr::set_builtin(*builtin),
            Self::FieldAccess(index) => Instr::set_field_at(stack_offset, *index),
            Self::TableIndex => Instr::set_table(stack_offset),
        }
    }
}
