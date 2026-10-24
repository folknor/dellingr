//! Fixed-width 64-bit register instruction encoding.
//!
//! Format: `[opcode:8][A:16][B:16][C:16][D:8]`, or `[opcode:8][A:16][B:16]
//! [sJ:24]` for the forms that carry a jump offset (the signed offset fills
//! the low 24 bits, overlapping C and D). Registers and literal-pool indices
//! are 16-bit operands; a constant operand is its own opcode, never a flag
//! bit. Each instruction is stored in one `u64`.

use crate::limits::{MAX_ARGUMENTS, MAX_FIXED_ARGS, MAX_FIXED_RESULTS, MAX_RESULTS};
use crate::numeral::{lua_add, lua_div, lua_modulo, lua_mul, lua_sub};

// The compiler caps fixed call arguments and results one below their runtime
// count ceilings. A return may still name MAX_RESULTS fixed values; only
// OPEN_COUNT denotes an open count operand.
const _: () = assert!(MAX_FIXED_ARGS + 1 == MAX_ARGUMENTS as usize);
const _: () = assert!(MAX_FIXED_RESULTS + 1 == MAX_RESULTS as usize);

/// Inline-cache slots available to one cache family of one function. Cache
/// operands are biased by one: operand 0 means uncached and operands 1..=255
/// name slots 0..=254, so a family holds at most this many slots and its
/// declared count (a `u8`) can never name an unencodable slot.
pub(crate) const MAX_CACHE_SLOTS: usize = u8::MAX as usize;

/// The count operand meaning "every value up to the top": an open call
/// argument list, all results, an open set-list batch or an open return.
pub(crate) const OPEN_COUNT: u16 = u16::MAX;

/// Largest jump offset magnitude: offsets are signed 24-bit values, and the
/// compiler rejects a jump at or past `2^23` instructions.
pub(crate) const MAX_JUMP_OFFSET: i32 = (1 << 23) - 1;

/// Encode a cache-slot operand: `None` (uncached) is 0, slot `n` is `n + 1`.
///
/// # Panics
///
/// Panics if the slot is not below [`MAX_CACHE_SLOTS`].
#[inline]
fn encode_cache_slot(slot: Option<u8>) -> u8 {
    match slot {
        None => 0,
        Some(slot) => {
            assert!(
                usize::from(slot) < MAX_CACHE_SLOTS,
                "cache slot must be below {MAX_CACHE_SLOTS}"
            );
            slot + 1
        }
    }
}

/// Decode a cache-slot operand written by [`encode_cache_slot`].
#[inline(always)]
const fn decode_cache_slot(operand: u8) -> Option<u8> {
    operand.checked_sub(1)
}

/// Argument count for function calls.
///
/// Bytecode supports fixed counts through `limits::MAX_FIXED_ARGS` or a
/// dynamic count. Host calls can use fixed counts through
/// `limits::MAX_ARGUMENTS`. The narrower bytecode cap leaves room for a
/// synthesized `__call` receiver within the runtime count width.
//
// A separate host-facing argument type (no `Dynamic`, no sentinel) was
// considered and declined. The maximum fixed host count is distinct from the
// dynamic bytecode operand: count operands are 16-bit and only `OPEN_COUNT`
// denotes a dynamic count. The `__call` path needs one value of headroom in
// the narrower fixed bytecode argument cap. Hosts naming `Dynamic` get a clean
// error from `call`/`call_anchor` before any state changes. A new type would
// only move that check to compile time, at the cost of breaking every host
// call site; revisit if the host API is redesigned for 1.0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgCount {
    /// Fixed number of arguments (through `limits::MAX_FIXED_ARGS` in
    /// bytecode, through `limits::MAX_ARGUMENTS` at runtime).
    Fixed(u8),
    /// Bytecode-only: every value from the first argument register up to the
    /// top an open producer left. Public host calls reject this variant; use
    /// [`ArgCount::Fixed`] instead.
    Dynamic,
}

impl ArgCount {
    /// Encode to a 16-bit count operand. Dynamic maps to [`OPEN_COUNT`].
    ///
    /// # Panics
    ///
    /// Panics on a fixed count above `limits::MAX_FIXED_ARGS`. Bytecode calls
    /// leave room below the runtime maximum for a `__call` receiver; hosts may
    /// still name the full runtime count.
    #[inline]
    pub(crate) fn to_operand(self) -> u16 {
        match self {
            ArgCount::Fixed(n) => {
                assert!(
                    usize::from(n) <= MAX_FIXED_ARGS,
                    "fixed bytecode argument count must be at most {MAX_FIXED_ARGS}"
                );
                u16::from(n)
            }
            ArgCount::Dynamic => OPEN_COUNT,
        }
    }

    /// Decode a 16-bit count operand. Values outside the `u8` range other than
    /// [`OPEN_COUNT`] never pass verification; they decode saturated.
    #[inline(always)]
    pub(crate) const fn from_operand(n: u16) -> Self {
        if n == OPEN_COUNT {
            ArgCount::Dynamic
        } else if n > u8::MAX as u16 {
            ArgCount::Fixed(u8::MAX)
        } else {
            ArgCount::Fixed(n as u8)
        }
    }
}

/// Return count for function calls and returns.
///
/// Bytecode supports fixed counts or all available values. Call results,
/// varargs and expanded assignments use `limits::MAX_FIXED_RESULTS`; a
/// `RETURN` may carry a fixed count through `limits::MAX_RESULTS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetCount {
    /// Fixed number of return values (through `limits::MAX_RESULTS`).
    Fixed(u8),
    /// Return/expect all available values.
    All,
}

impl RetCount {
    /// Encode to a 16-bit count operand. All maps to [`OPEN_COUNT`]; every
    /// fixed count is its own value.
    #[inline]
    pub(crate) const fn to_operand(self) -> u16 {
        match self {
            RetCount::Fixed(n) => n as u16,
            RetCount::All => OPEN_COUNT,
        }
    }

    /// Decode a 16-bit count operand. Values outside the `u8` range other than
    /// [`OPEN_COUNT`] never pass verification; they decode saturated.
    #[inline(always)]
    pub(crate) const fn from_operand(n: u16) -> Self {
        if n == OPEN_COUNT {
            RetCount::All
        } else if n > u8::MAX as u16 {
            RetCount::Fixed(u8::MAX)
        } else {
            RetCount::Fixed(n as u8)
        }
    }
}

macro_rules! define_builtins {
    ($( $variant:ident => $name:literal ),+ $(,)?) => {
        /// Well-known global names that get fast array-indexed access instead
        /// of a string lookup. Declaration order is persisted in builtin bytecode slots.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u8)]
        pub(crate) enum Builtin {
            $( $variant ),+
        }

        impl Builtin {
            /// Total number of builtin slots.
            pub(crate) const COUNT: usize = [$(stringify!($variant)),+].len();

            const ALL: [(Self, &'static str); Self::COUNT] = [
                $( (Self::$variant, $name) ),+
            ];

            /// Try to convert a global name to a Builtin slot.
            #[inline]
            pub(crate) fn from_name(name: &str) -> Option<Self> {
                match name {
                    $( $name => Some(Self::$variant), )+
                    _ => None,
                }
            }

            /// Get the name of this builtin.
            #[inline]
            pub(crate) const fn name(self) -> &'static str {
                match self {
                    $( Self::$variant => $name ),+
                }
            }

            /// Convert to u8 for instruction encoding.
            #[inline]
            pub(crate) const fn to_u8(self) -> u8 {
                self as u8
            }

            /// Convert from u8. Returns None if out of range.
            #[inline]
            pub(crate) fn from_u8(n: u8) -> Option<Self> {
                Self::ALL.get(usize::from(n)).map(|(builtin, _)| *builtin)
            }
        }
    };
}

// Keep this order stable: snapshot bytecode stores builtin indices directly.
define_builtins! {
    Print => "print",
    Type => "type",
    Tonumber => "tonumber",
    Tostring => "tostring",
    Pairs => "pairs",
    Ipairs => "ipairs",
    Next => "next",
    Getmetatable => "getmetatable",
    Setmetatable => "setmetatable",
    Rawget => "rawget",
    Rawset => "rawset",
    Rawequal => "rawequal",
    Rawlen => "rawlen",
    Select => "select",
    Unpack => "unpack",
    Math => "math",
    String => "string",
    Table => "table",
    G => "_G",
}

/// A 64-bit encoded instruction.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub(crate) struct Instr(u64);

// Keep the version in src root for both feature shapes; the opcode fixture reads it through instr.
#[path = "snapshot_format.rs"]
pub(crate) mod snapshot_format;

/// Static opcode charge categories. Native work is charged separately by its helpers.
#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpcodeCost {
    Arithmetic = 0,
    Negation,
    TableCreation,
    TableWrite,
    ArrayElements,
    Call,
    Free,
}

impl OpcodeCost {
    /// Number of categories with a reportable counter. `Free` has no counter.
    pub(crate) const COUNT: usize = Self::Free as usize;

    pub(crate) const fn counter_index(self) -> Option<usize> {
        match self {
            Self::Arithmetic
            | Self::Negation
            | Self::TableCreation
            | Self::TableWrite
            | Self::ArrayElements
            | Self::Call => Some(self as usize),
            Self::Free => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CostSummarySection {
    CostedOperations,
    Statistics,
}

pub(crate) struct OpcodeCostLabels {
    pub(crate) category: OpcodeCost,
    pub(crate) summary_prefix: &'static str,
    pub(crate) summary_suffix: &'static str,
    pub(crate) scope_label: &'static str,
    pub(crate) summary_section: CostSummarySection,
    pub(crate) summary_when_zero: bool,
}

// Kept in OpcodeCost discriminant order so a category's index addresses both
// the counter array and its labels. Keep Free last; each new category needs a row.
pub(crate) const OPCODE_COST_LABELS: [OpcodeCostLabels; OpcodeCost::COUNT] = [
    OpcodeCostLabels {
        category: OpcodeCost::Arithmetic,
        summary_prefix: "  Arithmetic (+,-,*,/,%,^): ",
        summary_suffix: " ops",
        scope_label: "arithmetic",
        summary_section: CostSummarySection::CostedOperations,
        summary_when_zero: false,
    },
    OpcodeCostLabels {
        category: OpcodeCost::Negation,
        summary_prefix: "  Unary negation (-):       ",
        summary_suffix: " ops",
        scope_label: "negation",
        summary_section: CostSummarySection::CostedOperations,
        summary_when_zero: false,
    },
    OpcodeCostLabels {
        category: OpcodeCost::TableCreation,
        summary_prefix: "  Table creation {}:        ",
        summary_suffix: " ops",
        scope_label: "table creation",
        summary_section: CostSummarySection::CostedOperations,
        summary_when_zero: false,
    },
    OpcodeCostLabels {
        category: OpcodeCost::TableWrite,
        summary_prefix: "  Table writes (t[k]=v):    ",
        summary_suffix: " ops",
        scope_label: "table writes",
        summary_section: CostSummarySection::CostedOperations,
        summary_when_zero: false,
    },
    OpcodeCostLabels {
        category: OpcodeCost::ArrayElements,
        summary_prefix: "  Array elements:           ",
        summary_suffix: " elements",
        scope_label: "array elements",
        summary_section: CostSummarySection::CostedOperations,
        summary_when_zero: false,
    },
    OpcodeCostLabels {
        category: OpcodeCost::Call,
        summary_prefix: "  Call instructions:    ",
        summary_suffix: "",
        scope_label: "call instructions",
        summary_section: CostSummarySection::Statistics,
        summary_when_zero: true,
    },
];

const _: () = assert!(OpcodeCost::Call as usize + 1 == OpcodeCost::COUNT);

#[cfg(test)]
mod opcode_cost_tests {
    use super::*;

    #[test]
    fn every_cost_category_has_labels_at_its_counter_index() {
        for (index, labels) in OPCODE_COST_LABELS.iter().enumerate() {
            assert_eq!(labels.category as usize, index);
            assert_eq!(labels.category.counter_index(), Some(index));
        }
        assert_eq!(OpcodeCost::Free.counter_index(), None);
        assert_eq!(OPCODE_COST_LABELS.len(), OpcodeCost::COUNT);
    }
}

/// The operand layout of an opcode, which fixes the reserved bits and the
/// disassembly shape. `Jump` forms carry the signed offset in the low 24 bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    /// A only.
    A,
    /// A and B.
    Ab,
    /// A, B and C.
    Abc,
    /// A, B and D.
    Abd,
    /// A, B, C and D.
    Abcd,
    /// A signed jump offset only.
    J,
    /// A and a signed jump offset.
    Aj,
    /// A, B and a signed jump offset.
    Abj,
}

impl Layout {
    /// The bits this layout leaves unused, which must be zero.
    const fn reserved(self) -> u64 {
        const A: u64 = 0xffff << 40;
        const B: u64 = 0xffff << 24;
        const C: u64 = 0xffff << 8;
        const D: u64 = 0xff;
        const J: u64 = 0xff_ffff;
        const OPERANDS: u64 = (1 << 56) - 1;
        let used = match self {
            Self::A => A,
            Self::Ab => A | B,
            Self::Abc => A | B | C,
            Self::Abd => A | B | D,
            Self::Abcd => A | B | C | D,
            Self::J => J,
            Self::Aj => A | J,
            Self::Abj => A | B | J,
        };
        OPERANDS & !used
    }
}

#[derive(Clone, Copy)]
struct OpcodeMetadata {
    layout: Layout,
    cost: OpcodeCost,
    arithmetic: Option<fn(f64, f64) -> f64>,
    foldable: bool,
}

macro_rules! opcode_arithmetic {
    () => {
        (None, false)
    };
    ($arithmetic:path, $foldable:literal) => {{
        let arithmetic: fn(f64, f64) -> f64 = $arithmetic;
        (Some(arithmetic), $foldable)
    }};
}

// Explicit numbers are snapshot data. Every opcode declares its operand
// layout (which fixes its reserved bits), static charge, and, for the
// arithmetic forms, the function the constant folder and the dispatch share.
macro_rules! opcode_rows {
    ($callback:ident) => {
        $callback! {
            OP_MOVE = 0, Ab, Free;
            OP_LOAD_NIL = 1, Ab, Free;
            OP_LOAD_BOOL = 2, Ab, Free;
            OP_LOAD_NUM = 3, Ab, Free;
            OP_LOAD_STR = 4, Ab, Free;
            OP_GET_UPVAL = 5, Ab, Free;
            OP_SET_UPVAL = 6, Ab, Free;
            OP_CLOSE = 7, A, Free;
            OP_GET_GLOBAL = 8, Abd, Free;
            OP_SET_GLOBAL = 9, Abd, Free;
            OP_GET_BUILTIN = 10, Ab, Free;
            OP_SET_BUILTIN = 11, Ab, Free;
            OP_NEW_TABLE = 12, A, TableCreation;
            OP_NEW_TABLE_PRESIZED = 13, Ab, TableCreation;
            OP_NEW_TABLE_TEMPLATE = 14, Ab, TableCreation;
            OP_GET_FIELD = 15, Abcd, Free;
            OP_GET_TABLE = 16, Abc, Free;
            OP_SET_FIELD = 17, Abcd, TableWrite;
            OP_SET_TABLE = 18, Abc, TableWrite;
            OP_INIT_FIELD = 19, Abc, TableWrite;
            OP_INIT_FIELD_PINNED = 20, Abcd, TableWrite;
            OP_INIT_INDEX = 21, Abc, TableWrite;
            OP_SET_LIST = 22, Abc, ArrayElements;
            OP_SELF = 23, Abcd, Free;
            OP_ADD_RR = 24, Abc, Arithmetic [lua_add, true];
            OP_ADD_RK = 25, Abc, Arithmetic [lua_add, true];
            OP_ADD_KR = 26, Abc, Arithmetic [lua_add, true];
            OP_SUB_RR = 27, Abc, Arithmetic [lua_sub, true];
            OP_SUB_RK = 28, Abc, Arithmetic [lua_sub, true];
            OP_SUB_KR = 29, Abc, Arithmetic [lua_sub, true];
            OP_MUL_RR = 30, Abc, Arithmetic [lua_mul, true];
            OP_MUL_RK = 31, Abc, Arithmetic [lua_mul, true];
            OP_MUL_KR = 32, Abc, Arithmetic [lua_mul, true];
            OP_DIV_RR = 33, Abc, Arithmetic [lua_div, true];
            OP_DIV_RK = 34, Abc, Arithmetic [lua_div, true];
            OP_DIV_KR = 35, Abc, Arithmetic [lua_div, true];
            OP_MOD_RR = 36, Abc, Arithmetic [lua_modulo, true];
            OP_MOD_RK = 37, Abc, Arithmetic [lua_modulo, true];
            OP_MOD_KR = 38, Abc, Arithmetic [lua_modulo, true];
            OP_POW_RR = 39, Abc, Arithmetic [crate::crmath::pow, false];
            OP_POW_RK = 40, Abc, Arithmetic [crate::crmath::pow, false];
            OP_POW_KR = 41, Abc, Arithmetic [crate::crmath::pow, false];
            OP_UNM = 42, Ab, Negation;
            OP_NOT = 43, Ab, Free;
            OP_LEN = 44, Ab, Free;
            OP_CONCAT = 45, Abc, Free;
            OP_LT = 46, Abc, Free;
            OP_LE = 47, Abc, Free;
            OP_GT = 48, Abc, Free;
            OP_GE = 49, Abc, Free;
            OP_EQ = 50, Abc, Free;
            OP_NE = 51, Abc, Free;
            OP_JMP = 52, J, Free;
            OP_BR_LT = 53, Abj, Free;
            OP_BR_LE = 54, Abj, Free;
            OP_BR_GT = 55, Abj, Free;
            OP_BR_GE = 56, Abj, Free;
            OP_BR_EQ = 57, Abj, Free;
            OP_BR_NE = 58, Abj, Free;
            OP_TEST_FALSE = 59, Aj, Free;
            OP_TEST_TRUE = 60, Aj, Free;
            OP_FOR_PREP = 61, Aj, Free;
            OP_FOR_LOOP = 62, Aj, Free;
            OP_TFOR_CALL = 63, Abd, Free;
            OP_TFOR_LOOP = 64, Aj, Free;
            OP_CALL = 65, Abcd, Call;
            OP_RETURN = 66, Ab, Free;
            OP_VARARG = 67, Ab, Free;
            OP_CLOSURE = 68, Ab, Free;
        }
    };
}

macro_rules! define_opcodes {
    (
        $(
            $name:ident = $number:literal, $layout:ident, $cost:ident
                $([$arithmetic:path, $foldable:literal])?;
        )+
    ) => {
        impl Instr {
            $( pub(crate) const $name: u8 = $number; )+

            const OPCODES: [Option<OpcodeMetadata>; 256] = {
                let mut entries = [None; 256];
                $(
                    assert!(entries[$number].is_none());
                    let (arithmetic, foldable) =
                        opcode_arithmetic!($($arithmetic, $foldable)?);
                    entries[$number] = Some(OpcodeMetadata {
                        layout: Layout::$layout,
                        cost: OpcodeCost::$cost,
                        arithmetic,
                        foldable,
                    });
                )+
                entries
            };

            /// Every opcode's name, in declaration order.
            pub(crate) const OPCODE_NAMES: &'static [(u8, &'static str)] = &[
                $( (Self::$name, stringify!($name)), )+
            ];
        }
    };
}

opcode_rows!(define_opcodes);

impl Instr {
    pub(crate) const fn known_opcode(opcode: u8) -> bool {
        Self::OPCODES[opcode as usize].is_some()
    }

    /// The operand layout of a known opcode.
    pub(crate) const fn layout(opcode: u8) -> Option<Layout> {
        match Self::OPCODES[opcode as usize] {
            Some(metadata) => Some(metadata.layout),
            None => None,
        }
    }

    /// Whether every bit the opcode's layout leaves unused is zero.
    pub(crate) const fn reserved_bits_zero(self) -> bool {
        match Self::OPCODES[self.opcode() as usize] {
            Some(metadata) => self.0 & metadata.layout.reserved() == 0,
            None => false,
        }
    }

    /// Whether the opcode carries a jump offset.
    pub(crate) const fn is_transfer(opcode: u8) -> bool {
        matches!(
            Self::layout(opcode),
            Some(Layout::J | Layout::Aj | Layout::Abj)
        )
    }

    pub(crate) const fn cost_class(opcode: u8) -> OpcodeCost {
        match Self::OPCODES[opcode as usize] {
            Some(metadata) => metadata.cost,
            None => OpcodeCost::Free,
        }
    }

    /// The arithmetic function of a foldable arithmetic opcode.
    pub(crate) fn arithmetic_fn(opcode: u8) -> Option<fn(f64, f64) -> f64> {
        match Self::OPCODES[opcode as usize] {
            Some(metadata) if metadata.foldable => metadata.arithmetic,
            _ => None,
        }
    }

    /// The arithmetic function of any arithmetic opcode, `^` included.
    pub(crate) fn arithmetic_op(opcode: u8) -> Option<fn(f64, f64) -> f64> {
        match Self::OPCODES[opcode as usize] {
            Some(metadata) => metadata.arithmetic,
            None => None,
        }
    }

    /// The fixed charge of an opcode: 1 for the costed classes, 0 otherwise.
    /// `SET_LIST`'s per-element charge is not fixed and counts 0 here.
    pub(crate) const fn fixed_units(opcode: u8) -> u64 {
        match Self::cost_class(opcode) {
            OpcodeCost::Arithmetic
            | OpcodeCost::Negation
            | OpcodeCost::TableCreation
            | OpcodeCost::TableWrite => 1,
            OpcodeCost::ArrayElements | OpcodeCost::Free | OpcodeCost::Call => 0,
        }
    }

    /// Fixed opcode units, or the encoded list length for static analysis.
    /// An open SET_LIST counts zero statically; execution charges the live
    /// list length instead. Native charges are not included.
    pub(crate) const fn static_units(self) -> u64 {
        match Self::cost_class(self.opcode()) {
            OpcodeCost::Arithmetic
            | OpcodeCost::Negation
            | OpcodeCost::TableCreation
            | OpcodeCost::TableWrite => Self::fixed_units(self.opcode()),
            OpcodeCost::ArrayElements => {
                if self.b() == OPEN_COUNT {
                    0
                } else {
                    self.b() as u64
                }
            }
            OpcodeCost::Free | OpcodeCost::Call => 0,
        }
    }
}

/// Which operand of a binary arithmetic opcode is a number literal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArithOperands {
    /// `R[B] op R[C]`.
    RegReg,
    /// `R[B] op K[C]`.
    RegConst,
    /// `K[B] op R[C]`.
    ConstReg,
}

/// The binary arithmetic operators, in opcode-row order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
}

impl ArithOp {
    const fn base(self) -> u8 {
        match self {
            Self::Add => Instr::OP_ADD_RR,
            Self::Sub => Instr::OP_SUB_RR,
            Self::Mul => Instr::OP_MUL_RR,
            Self::Div => Instr::OP_DIV_RR,
            Self::Mod => Instr::OP_MOD_RR,
            Self::Pow => Instr::OP_POW_RR,
        }
    }

    /// The opcode of this operator with the given operand kinds.
    pub(crate) const fn opcode(self, operands: ArithOperands) -> u8 {
        self.base()
            + match operands {
                ArithOperands::RegReg => 0,
                ArithOperands::RegConst => 1,
                ArithOperands::ConstReg => 2,
            }
    }

    /// The function the dispatch and the constant folder apply.
    pub(crate) fn function(self) -> fn(f64, f64) -> f64 {
        Instr::arithmetic_op(self.base()).expect("every arithmetic row declares its function")
    }

    /// Whether the constant folder may evaluate this operator. `^` is never
    /// folded: a folded literal is free while a runtime `^` is charged.
    pub(crate) fn foldable(self) -> bool {
        Instr::arithmetic_fn(self.base()).is_some()
    }

    /// Decode an arithmetic opcode into its operator and operand kinds.
    pub(crate) const fn decode(opcode: u8) -> Option<(Self, ArithOperands)> {
        if opcode < Instr::OP_ADD_RR || opcode > Instr::OP_POW_KR {
            return None;
        }
        let index = opcode - Instr::OP_ADD_RR;
        let op = match index / 3 {
            0 => Self::Add,
            1 => Self::Sub,
            2 => Self::Mul,
            3 => Self::Div,
            4 => Self::Mod,
            _ => Self::Pow,
        };
        let operands = match index % 3 {
            0 => ArithOperands::RegReg,
            1 => ArithOperands::RegConst,
            _ => ArithOperands::ConstReg,
        };
        Some((op, operands))
    }
}

/// The comparison operators shared by the value and branch forms. The
/// inclusive orderings negate the opposite strict ordering, so `<=` on an
/// incomparable pair (NaN) is false like `<`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompareOp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

impl CompareOp {
    /// The value-producing opcode.
    pub(crate) const fn value_opcode(self) -> u8 {
        Instr::OP_LT + self as u8
    }

    /// The branch-if-false opcode.
    pub(crate) const fn branch_opcode(self) -> u8 {
        Instr::OP_BR_LT + self as u8
    }

    /// Decode a value comparison opcode.
    pub(crate) const fn from_value_opcode(opcode: u8) -> Option<Self> {
        Self::from_index(opcode.wrapping_sub(Instr::OP_LT))
    }

    /// Decode a branch comparison opcode.
    pub(crate) const fn from_branch_opcode(opcode: u8) -> Option<Self> {
        Self::from_index(opcode.wrapping_sub(Instr::OP_BR_LT))
    }

    const fn from_index(index: u8) -> Option<Self> {
        Some(match index {
            0 => Self::Lt,
            1 => Self::Le,
            2 => Self::Gt,
            3 => Self::Ge,
            4 => Self::Eq,
            5 => Self::Ne,
            _ => return None,
        })
    }

    /// For the ordering comparisons, the target ordering and whether the
    /// test is negated; `None` for equality.
    pub(crate) const fn ordering(self) -> Option<(std::cmp::Ordering, bool)> {
        use std::cmp::Ordering;
        match self {
            Self::Lt => Some((Ordering::Less, false)),
            Self::Le => Some((Ordering::Greater, true)),
            Self::Gt => Some((Ordering::Greater, false)),
            Self::Ge => Some((Ordering::Less, true)),
            Self::Eq | Self::Ne => None,
        }
    }
}

const _: () = {
    assert!(Instr::OP_LE == Instr::OP_LT + 1 && Instr::OP_NE == Instr::OP_LT + 5);
    assert!(Instr::OP_BR_LE == Instr::OP_BR_LT + 1 && Instr::OP_BR_NE == Instr::OP_BR_LT + 5);
    assert!(Instr::OP_POW_KR == Instr::OP_ADD_RR + 17);
};

// Raw field access. Typed decoders below name what each field means for the
// opcodes that use it; the VM, the verifier, finalization and the
// disassembler read sentinel-bearing operands only through those.
impl Instr {
    const A_SHIFT: u32 = 40;
    const B_SHIFT: u32 = 24;
    const C_SHIFT: u32 = 8;

    #[inline]
    const fn pack(opcode: u8, a: u16, b: u16, c: u16, d: u8) -> Self {
        Instr(
            (opcode as u64) << 56
                | (a as u64) << Self::A_SHIFT
                | (b as u64) << Self::B_SHIFT
                | (c as u64) << Self::C_SHIFT
                | d as u64,
        )
    }

    #[inline]
    const fn pack_jump(opcode: u8, a: u16, b: u16, offset: i32) -> Self {
        Instr(
            (opcode as u64) << 56
                | (a as u64) << Self::A_SHIFT
                | (b as u64) << Self::B_SHIFT
                | (offset as u32 as u64 & 0xff_ffff),
        )
    }

    /// The opcode.
    #[inline(always)]
    pub(crate) const fn opcode(self) -> u8 {
        (self.0 >> 56) as u8
    }

    /// The A operand.
    #[inline(always)]
    pub(crate) const fn a(self) -> u16 {
        (self.0 >> Self::A_SHIFT) as u16
    }

    /// The B operand.
    #[inline(always)]
    pub(crate) const fn b(self) -> u16 {
        (self.0 >> Self::B_SHIFT) as u16
    }

    /// The C operand.
    #[inline(always)]
    pub(crate) const fn c(self) -> u16 {
        (self.0 >> Self::C_SHIFT) as u16
    }

    /// The D operand.
    #[inline(always)]
    pub(crate) const fn d(self) -> u8 {
        self.0 as u8
    }

    /// The signed 24-bit jump offset of a transfer opcode, relative to the
    /// next instruction.
    #[inline(always)]
    pub(crate) const fn sj(self) -> i32 {
        ((self.0 as u32) << 8) as i32 >> 8
    }

    /// The same instruction with its jump offset replaced.
    #[inline]
    pub(crate) const fn with_offset(self, offset: i32) -> Self {
        Self::pack_jump(self.opcode(), self.a(), self.b(), offset)
    }

    /// The same instruction with its A operand replaced.
    #[inline]
    pub(crate) const fn with_a(self, a: u16) -> Self {
        Instr(self.0 & !(0xffff << Self::A_SHIFT) | (a as u64) << Self::A_SHIFT)
    }

    /// The same instruction with its B operand replaced.
    #[inline]
    pub(crate) const fn with_b(self, b: u16) -> Self {
        Instr(self.0 & !(0xffff << Self::B_SHIFT) | (b as u64) << Self::B_SHIFT)
    }

    /// The same instruction with its C operand replaced.
    #[inline]
    pub(crate) const fn with_c(self, c: u16) -> Self {
        Instr(self.0 & !(0xffff << Self::C_SHIFT) | (c as u64) << Self::C_SHIFT)
    }

    /// Get the raw u64 value.
    #[inline]
    pub(crate) const fn raw(self) -> u64 {
        self.0
    }

    /// Rebuild an instruction from its raw 64-bit encoding.
    #[inline]
    pub(crate) const fn from_raw(raw: u64) -> Self {
        Instr(raw)
    }
}

// Constructors. Registers, literal and nested indices are u16 operands; the
// verifier bounds each against its function.
impl Instr {
    pub(crate) const fn mov(dst: u16, src: u16) -> Self {
        Self::pack(Self::OP_MOVE, dst, src, 0, 0)
    }
    /// Set `count` registers from `dst` to nil (`count >= 1`).
    pub(crate) const fn load_nil(dst: u16, count: u16) -> Self {
        Self::pack(Self::OP_LOAD_NIL, dst, count, 0, 0)
    }
    pub(crate) const fn load_bool(dst: u16, value: bool) -> Self {
        Self::pack(Self::OP_LOAD_BOOL, dst, value as u16, 0, 0)
    }
    pub(crate) const fn load_num(dst: u16, literal: u16) -> Self {
        Self::pack(Self::OP_LOAD_NUM, dst, literal, 0, 0)
    }
    pub(crate) const fn load_str(dst: u16, literal: u16) -> Self {
        Self::pack(Self::OP_LOAD_STR, dst, literal, 0, 0)
    }
    pub(crate) const fn get_upval(dst: u16, upvalue: u16) -> Self {
        Self::pack(Self::OP_GET_UPVAL, dst, upvalue, 0, 0)
    }
    pub(crate) const fn set_upval(src: u16, upvalue: u16) -> Self {
        Self::pack(Self::OP_SET_UPVAL, src, upvalue, 0, 0)
    }
    /// Close every open upvalue at or above register `level`.
    pub(crate) const fn close(level: u16) -> Self {
        Self::pack(Self::OP_CLOSE, level, 0, 0, 0)
    }

    // Cache operands are serialized as raw instruction words, so changing
    // their encoding requires a snapshot format version bump. Every cache
    // operand lives in D and uses the one biased encoding of
    // `encode_cache_slot`: 0 is uncached.
    pub(crate) fn get_global(dst: u16, name: u16, cache_slot: Option<u8>) -> Self {
        Self::pack(
            Self::OP_GET_GLOBAL,
            dst,
            name,
            0,
            encode_cache_slot(cache_slot),
        )
    }
    pub(crate) fn set_global(src: u16, name: u16, cache_slot: Option<u8>) -> Self {
        Self::pack(
            Self::OP_SET_GLOBAL,
            src,
            name,
            0,
            encode_cache_slot(cache_slot),
        )
    }
    /// The cache slot of a cache-bearing opcode (GET_GLOBAL, SET_GLOBAL,
    /// GET_FIELD, SET_FIELD, SELF, TFOR_CALL).
    #[inline(always)]
    pub(crate) const fn cache_slot(self) -> Option<u8> {
        decode_cache_slot(self.d())
    }
    /// The same instruction with its cache operand replaced.
    pub(crate) fn with_cache_slot(self, slot: Option<u8>) -> Self {
        Instr(self.0 & !0xff | u64::from(encode_cache_slot(slot)))
    }
    pub(crate) const fn get_builtin(dst: u16, builtin: Builtin) -> Self {
        Self::pack(Self::OP_GET_BUILTIN, dst, builtin.to_u8() as u16, 0, 0)
    }
    pub(crate) const fn set_builtin(src: u16, builtin: Builtin) -> Self {
        Self::pack(Self::OP_SET_BUILTIN, src, builtin.to_u8() as u16, 0, 0)
    }

    pub(crate) const fn new_table(dst: u16) -> Self {
        Self::pack(Self::OP_NEW_TABLE, dst, 0, 0, 0)
    }
    /// A table whose hash part is presized for `capacity` keyed fields.
    pub(crate) const fn new_table_presized(dst: u16, capacity: u8) -> Self {
        Self::pack(Self::OP_NEW_TABLE_PRESIZED, dst, capacity as u16, 0, 0)
    }
    pub(crate) const fn new_table_template(dst: u16, template: u8) -> Self {
        Self::pack(Self::OP_NEW_TABLE_TEMPLATE, dst, template as u16, 0, 0)
    }
    /// `R[dst] = R[table].name`.
    pub(crate) fn get_field(dst: u16, table: u16, name: u16, cache_slot: Option<u8>) -> Self {
        Self::pack(
            Self::OP_GET_FIELD,
            dst,
            table,
            name,
            encode_cache_slot(cache_slot),
        )
    }
    /// `R[dst] = R[table][R[key]]`.
    pub(crate) const fn get_table(dst: u16, table: u16, key: u16) -> Self {
        Self::pack(Self::OP_GET_TABLE, dst, table, key, 0)
    }
    /// `R[table].name = R[src]`.
    pub(crate) fn set_field(table: u16, name: u16, src: u16, cache_slot: Option<u8>) -> Self {
        Self::pack(
            Self::OP_SET_FIELD,
            table,
            name,
            src,
            encode_cache_slot(cache_slot),
        )
    }
    /// `R[table][R[key]] = R[src]`.
    pub(crate) const fn set_table(table: u16, key: u16, src: u16) -> Self {
        Self::pack(Self::OP_SET_TABLE, table, key, src, 0)
    }
    /// A constructor's `name = R[src]` on the fresh table `R[table]`.
    pub(crate) const fn init_field(table: u16, name: u16, src: u16) -> Self {
        Self::pack(Self::OP_INIT_FIELD, table, name, src, 0)
    }
    /// A template constructor's field write by pinned entry index.
    pub(crate) const fn init_field_pinned(table: u16, name: u16, src: u16, entry: u8) -> Self {
        Self::pack(Self::OP_INIT_FIELD_PINNED, table, name, src, entry)
    }
    /// A constructor's `[R[key]] = R[src]` on the fresh table `R[table]`.
    pub(crate) const fn init_index(table: u16, key: u16, src: u16) -> Self {
        Self::pack(Self::OP_INIT_INDEX, table, key, src, 0)
    }
    /// Store `count` positional values from `R[table + 1..]` (or every value
    /// up to the top for `None`) as batch `batch` of the constructor.
    pub(crate) const fn set_list(table: u16, count: Option<u8>, batch: u16) -> Self {
        let count = match count {
            Some(count) => count as u16,
            None => OPEN_COUNT,
        };
        Self::pack(Self::OP_SET_LIST, table, count, batch, 0)
    }
    /// The positional count of a SET_LIST, `None` when open.
    #[inline(always)]
    pub(crate) const fn set_list_count(self) -> Option<u16> {
        if self.b() == OPEN_COUNT {
            None
        } else {
            Some(self.b())
        }
    }
    /// `R[dst + 1] = R[object]; R[dst] = R[object].name` for a method call.
    pub(crate) fn self_(dst: u16, object: u16, name: u16, cache_slot: Option<u8>) -> Self {
        Self::pack(
            Self::OP_SELF,
            dst,
            object,
            name,
            encode_cache_slot(cache_slot),
        )
    }

    /// A binary arithmetic instruction. Constant operands are number-literal
    /// indices as `operands` says.
    pub(crate) const fn arith(
        op: ArithOp,
        operands: ArithOperands,
        dst: u16,
        b: u16,
        c: u16,
    ) -> Self {
        Self::pack(op.opcode(operands), dst, b, c, 0)
    }
    pub(crate) const fn unm(dst: u16, src: u16) -> Self {
        Self::pack(Self::OP_UNM, dst, src, 0, 0)
    }
    pub(crate) const fn not(dst: u16, src: u16) -> Self {
        Self::pack(Self::OP_NOT, dst, src, 0, 0)
    }
    pub(crate) const fn len(dst: u16, src: u16) -> Self {
        Self::pack(Self::OP_LEN, dst, src, 0, 0)
    }
    /// `R[dst] = R[first] .. ... .. R[first + count - 1]` (`count >= 2`).
    pub(crate) const fn concat(dst: u16, first: u16, count: u16) -> Self {
        Self::pack(Self::OP_CONCAT, dst, first, count, 0)
    }
    /// `R[dst] = R[lhs] op R[rhs]` as a boolean.
    pub(crate) const fn compare(op: CompareOp, dst: u16, lhs: u16, rhs: u16) -> Self {
        Self::pack(op.value_opcode(), dst, lhs, rhs, 0)
    }

    pub(crate) const fn jump(offset: i32) -> Self {
        Self::pack_jump(Self::OP_JMP, 0, 0, offset)
    }
    /// Jump by `offset` when `R[lhs] op R[rhs]` is false.
    pub(crate) const fn branch_compare(op: CompareOp, lhs: u16, rhs: u16, offset: i32) -> Self {
        Self::pack_jump(op.branch_opcode(), lhs, rhs, offset)
    }
    /// Jump by `offset` when `R[reg]` is falsy.
    pub(crate) const fn test_false(reg: u16, offset: i32) -> Self {
        Self::pack_jump(Self::OP_TEST_FALSE, reg, 0, offset)
    }
    /// Jump by `offset` when `R[reg]` is truthy.
    pub(crate) const fn test_true(reg: u16, offset: i32) -> Self {
        Self::pack_jump(Self::OP_TEST_TRUE, reg, 0, offset)
    }
    /// Numeric-for setup over `R[base..base + 4)`: start, limit, step and the
    /// visible variable. Jumps past the body when the loop does not run.
    pub(crate) const fn for_prep(base: u16, offset: i32) -> Self {
        Self::pack_jump(Self::OP_FOR_PREP, base, 0, offset)
    }
    pub(crate) const fn for_loop(base: u16, offset: i32) -> Self {
        Self::pack_jump(Self::OP_FOR_LOOP, base, 0, offset)
    }
    /// Call the generic-for iterator in `R[base]` with `R[base + 1]` and
    /// `R[base + 2]`, writing `vars` results from `R[base + 3]`.
    pub(crate) fn tfor_call(base: u16, vars: u8, cursor_slot: Option<u8>) -> Self {
        Self::pack(
            Self::OP_TFOR_CALL,
            base,
            vars as u16,
            0,
            encode_cache_slot(cursor_slot),
        )
    }
    pub(crate) const fn tfor_loop(base: u16, offset: i32) -> Self {
        Self::pack_jump(Self::OP_TFOR_LOOP, base, 0, offset)
    }

    /// Call `R[base]` with the arguments from `R[base + 1]`. A method call
    /// (`obj:m()`) is marked: the mark changes nothing at execution; a
    /// failing library function reads it to number its arguments the way
    /// the script author wrote them.
    pub(crate) fn call(base: u16, args: ArgCount, results: RetCount, method: bool) -> Self {
        Self::pack(
            Self::OP_CALL,
            base,
            args.to_operand(),
            results.to_operand(),
            u8::from(method),
        )
    }
    /// The argument count of an `OP_CALL`.
    #[inline(always)]
    pub(crate) const fn call_args(self) -> ArgCount {
        ArgCount::from_operand(self.b())
    }
    /// The result count an `OP_CALL` asks for.
    #[inline(always)]
    pub(crate) const fn call_results(self) -> RetCount {
        RetCount::from_operand(self.c())
    }
    /// Whether an `OP_CALL` is the call of a method call (`obj:m()`).
    #[inline(always)]
    pub(crate) const fn call_is_method(self) -> bool {
        self.d() != 0
    }
    /// Return `count` values from `R[first]`, or every value up to the top.
    pub(crate) const fn ret(first: u16, count: RetCount) -> Self {
        Self::pack(Self::OP_RETURN, first, count.to_operand(), 0, 0)
    }
    /// The result count of an `OP_RETURN`.
    #[inline(always)]
    pub(crate) const fn return_count(self) -> RetCount {
        RetCount::from_operand(self.b())
    }
    /// Copy `count` varargs to `R[dst..]` padded with nil, or every vararg.
    pub(crate) fn vararg(dst: u16, count: RetCount) -> Self {
        Self::pack(Self::OP_VARARG, dst, count.to_operand(), 0, 0)
    }
    /// The value count of an `OP_VARARG`.
    #[inline(always)]
    pub(crate) const fn vararg_count(self) -> RetCount {
        RetCount::from_operand(self.b())
    }
    pub(crate) const fn closure(dst: u16, nested: u16) -> Self {
        Self::pack(Self::OP_CLOSURE, dst, nested, 0, 0)
    }

    /// Whether this instruction leaves an open top for the next instruction
    /// to consume: a call asking for all results or a vararg of every value.
    pub(crate) const fn produces_open(self) -> bool {
        match self.opcode() {
            Self::OP_CALL => self.c() == OPEN_COUNT,
            Self::OP_VARARG => self.b() == OPEN_COUNT,
            _ => false,
        }
    }

    /// Whether this instruction reads an open top left by the instruction
    /// before it: a call with open arguments, an open return or set-list.
    pub(crate) const fn consumes_open(self) -> bool {
        match self.opcode() {
            Self::OP_CALL | Self::OP_RETURN | Self::OP_SET_LIST => self.b() == OPEN_COUNT,
            _ => false,
        }
    }
}

impl std::fmt::Debug for Instr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let opcode = self.opcode();
        let Some(&(_, name)) = Self::OPCODE_NAMES.iter().find(|(op, _)| *op == opcode) else {
            return write!(f, "Unknown(op={}, raw={:#x})", opcode, self.0);
        };
        let name = name.trim_start_matches("OP_");
        let layout = Self::layout(opcode).expect("named opcodes have a layout");
        match layout {
            Layout::A => write!(f, "{name} {}", self.a()),
            Layout::Ab => write!(f, "{name} {} {}", self.a(), self.b()),
            Layout::Abc => write!(f, "{name} {} {} {}", self.a(), self.b(), self.c()),
            Layout::Abd => write!(f, "{name} {} {} d{}", self.a(), self.b(), self.d()),
            Layout::Abcd => write!(
                f,
                "{name} {} {} {} d{}",
                self.a(),
                self.b(),
                self.c(),
                self.d()
            ),
            Layout::J => write!(f, "{name} {:+}", self.sj()),
            Layout::Aj => write!(f, "{name} {} {:+}", self.a(), self.sj()),
            Layout::Abj => write!(f, "{name} {} {} {:+}", self.a(), self.b(), self.sj()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ArgCount, ArithOp, ArithOperands, Builtin, CompareOp, Instr, OPEN_COUNT, RetCount,
    };

    // The hand-maintained fixture records the opcode rows' replay-visible
    // number, operand layout and cost class, plus builtin slots and persisted
    // cache and jump encodings. It is deliberately independent of the
    // declaration tables: a snapshot-format or cost-model change must review
    // this whole contract, holes included, rather than bless regenerated rows.
    #[test]
    fn persisted_encodings_match_fixture() {
        use std::fmt::Write;

        let golden = include_str!("../tests/fixtures/opcode_encoding.txt");
        let (version, remainder) = golden.split_once('\n').expect("fixture has a format line");
        assert_eq!(
            version,
            format!("format={}", super::snapshot_format::FORMAT_VERSION)
        );
        let (cost_model, expected) = remainder
            .split_once('\n')
            .expect("fixture has a cost model version line");
        assert_eq!(
            cost_model,
            format!("cost_model={}", crate::COST_MODEL_VERSION)
        );
        let mut actual = String::new();
        for (opcode, name) in Instr::OPCODE_NAMES {
            writeln!(
                actual,
                "{name}={opcode} {:?} {:?}",
                Instr::layout(*opcode).expect("opcode names list only known opcodes"),
                Instr::cost_class(*opcode)
            )
            .expect("writing into String cannot fail");
        }
        for (builtin, name) in Builtin::ALL {
            writeln!(actual, "builtin:{name}={}", builtin.to_u8())
                .expect("writing into String cannot fail");
        }
        // Cache operands are persisted inside raw instruction words: pin the
        // uncached, first and last slot encodings of every cache family.
        type CacheFamily = (&'static str, fn(Option<u8>) -> Instr);
        let cache_families: [CacheFamily; 6] = [
            ("OP_GET_GLOBAL", |s| Instr::get_global(1, 7, s)),
            ("OP_SET_GLOBAL", |s| Instr::set_global(1, 7, s)),
            ("OP_GET_FIELD", |s| Instr::get_field(1, 2, 7, s)),
            ("OP_SET_FIELD", |s| Instr::set_field(1, 7, 2, s)),
            ("OP_SELF", |s| Instr::self_(1, 2, 7, s)),
            ("OP_TFOR_CALL", |s| Instr::tfor_call(3, 2, s)),
        ];
        for (name, encode) in cache_families {
            writeln!(
                actual,
                "cache:{name}={:016x},{:016x},{:016x}",
                encode(None).raw(),
                encode(Some(0)).raw(),
                encode(Some(254)).raw()
            )
            .expect("writing into String cannot fail");
        }
        writeln!(
            actual,
            "jump:{:016x},{:016x}",
            Instr::jump(-1).raw(),
            Instr::branch_compare(CompareOp::Lt, 1, 2, super::MAX_JUMP_OFFSET).raw()
        )
        .expect("writing into String cannot fail");
        assert_eq!(
            actual, expected,
            "snapshot opcode, cost-model, or builtin encoding changed"
        );

        #[cfg(feature = "snapshot")]
        {
            let saved = crate::State::new()
                .save_state()
                .expect("fresh state can be saved");
            assert_eq!(&saved.bytes[..4], b"DLGS");
            let format = u16::from_le_bytes([saved.bytes[4], saved.bytes[5]]);
            assert_eq!(
                format!("format={format}"),
                version,
                "save header layout must expose the fixture format at bytes 4..6"
            );
        }
    }

    #[test]
    fn count_encoding_round_trips_without_fixed_sentinel_aliases() {
        for n in 0..u8::MAX {
            assert_eq!(
                ArgCount::from_operand(ArgCount::Fixed(n).to_operand()),
                ArgCount::Fixed(n)
            );
        }
        for n in 0..=u8::MAX {
            assert_eq!(
                RetCount::from_operand(RetCount::Fixed(n).to_operand()),
                RetCount::Fixed(n)
            );
        }
        assert_eq!(ArgCount::from_operand(OPEN_COUNT), ArgCount::Dynamic);
        assert_eq!(RetCount::from_operand(OPEN_COUNT), RetCount::All);
    }

    #[test]
    fn arithmetic_opcodes_round_trip_and_share_one_function_per_operator() {
        type ArithFn = fn(f64, f64) -> f64;
        let operators: [(ArithOp, ArithFn); 6] = [
            (ArithOp::Add, crate::numeral::lua_add),
            (ArithOp::Sub, crate::numeral::lua_sub),
            (ArithOp::Mul, crate::numeral::lua_mul),
            (ArithOp::Div, crate::numeral::lua_div),
            (ArithOp::Mod, crate::numeral::lua_modulo),
            (ArithOp::Pow, crate::crmath::pow),
        ];
        for (op, expected) in operators {
            for operands in [
                ArithOperands::RegReg,
                ArithOperands::RegConst,
                ArithOperands::ConstReg,
            ] {
                let opcode = op.opcode(operands);
                assert_eq!(ArithOp::decode(opcode), Some((op, operands)));
                let actual = Instr::arithmetic_op(opcode).expect("arithmetic row");
                assert_eq!(actual(4.5, -2.0).to_bits(), expected(4.5, -2.0).to_bits());
                assert_eq!(
                    Instr::arithmetic_fn(opcode).is_some(),
                    op != ArithOp::Pow,
                    "only ^ is unfoldable"
                );
            }
        }
        assert!(ArithOp::decode(Instr::OP_UNM).is_none());
    }

    #[test]
    fn comparison_opcodes_round_trip() {
        for op in [
            CompareOp::Lt,
            CompareOp::Le,
            CompareOp::Gt,
            CompareOp::Ge,
            CompareOp::Eq,
            CompareOp::Ne,
        ] {
            assert_eq!(CompareOp::from_value_opcode(op.value_opcode()), Some(op));
            assert_eq!(CompareOp::from_branch_opcode(op.branch_opcode()), Some(op));
        }
        assert_eq!(CompareOp::from_value_opcode(Instr::OP_JMP), None);
    }

    #[test]
    fn jump_offsets_sign_extend_through_24_bits() {
        for offset in [
            0,
            1,
            -1,
            12345,
            -12345,
            super::MAX_JUMP_OFFSET,
            -super::MAX_JUMP_OFFSET - 1,
        ] {
            let inst = Instr::branch_compare(CompareOp::Ge, 0xffff, 0xfffe, offset);
            assert_eq!(inst.sj(), offset);
            assert_eq!((inst.a(), inst.b()), (0xffff, 0xfffe));
            assert!(inst.reserved_bits_zero());
            assert_eq!(
                inst.with_offset(-offset.min(super::MAX_JUMP_OFFSET)).a(),
                0xffff
            );
        }
    }

    #[test]
    #[should_panic(expected = "fixed bytecode argument count must be at most 254")]
    fn fixed_sentinel_argument_cannot_be_encoded() {
        let _ = Instr::call(0, ArgCount::Fixed(255), RetCount::Fixed(0), false);
    }

    #[test]
    #[should_panic(expected = "cache slot must be below 255")]
    fn cache_slot_255_cannot_be_encoded() {
        let _ = Instr::get_field(0, 0, 0, Some(u8::MAX));
    }

    // Snapshot data: every cache operand is biased by one with 0 = uncached,
    // so the largest slot (254) encodes as 255 and no slot aliases "uncached".
    #[test]
    fn cache_operands_share_one_biased_encoding() {
        let families: [fn(Option<u8>) -> Instr; 6] = [
            |s| Instr::get_global(1, 7, s),
            |s| Instr::set_global(1, 7, s),
            |s| Instr::get_field(1, 2, 7, s),
            |s| Instr::set_field(1, 7, 2, s),
            |s| Instr::self_(1, 2, 7, s),
            |s| Instr::tfor_call(3, 2, s),
        ];
        assert_eq!(super::MAX_CACHE_SLOTS, 255);
        for encode in families {
            assert_eq!(encode(None).d(), 0);
            assert_eq!(encode(None).cache_slot(), None);
            for slot in 0..u8::MAX {
                let inst = encode(Some(slot));
                assert_eq!(inst.d(), slot + 1);
                assert_eq!(inst.cache_slot(), Some(slot));
                assert_eq!(encode(None).with_cache_slot(Some(slot)), inst);
                assert!(inst.reserved_bits_zero());
            }
        }
        // The operands that are not the cache are untouched by the encoding.
        assert_eq!(Instr::get_global(1, 7, Some(3)).b(), 7);
        let tfor = Instr::tfor_call(3, 2, Some(9));
        assert_eq!((tfor.a(), tfor.b()), (3, 2));
    }

    #[test]
    fn open_counts_mark_producers_and_consumers() {
        assert!(Instr::call(4, ArgCount::Fixed(1), RetCount::All, false).produces_open());
        assert!(Instr::vararg(4, RetCount::All).produces_open());
        assert!(!Instr::vararg(4, RetCount::Fixed(2)).produces_open());
        assert!(Instr::call(4, ArgCount::Dynamic, RetCount::Fixed(1), false).consumes_open());
        assert!(Instr::ret(4, RetCount::All).consumes_open());
        assert!(Instr::set_list(4, None, 0).consumes_open());
        assert!(!Instr::set_list(4, Some(3), 0).consumes_open());
        assert_eq!(
            Instr::ret(0, RetCount::Fixed(255)).return_count(),
            RetCount::Fixed(255)
        );
    }

    #[test]
    fn set_list_static_units_count_fixed_batches_only() {
        assert_eq!(Instr::set_list(0, Some(7), 0).static_units(), 7);
        assert_eq!(Instr::set_list(0, None, 0).static_units(), 0);
    }

    #[test]
    fn builtin_slots_round_trip_through_names_and_bytes() {
        assert_eq!(Builtin::COUNT, Builtin::ALL.len());
        for (index, (builtin, name)) in Builtin::ALL.iter().enumerate() {
            assert_eq!(usize::from(builtin.to_u8()), index);
            assert_eq!(builtin.name(), *name);
            assert_eq!(Builtin::from_name(name), Some(*builtin));
        }

        for raw in u8::MIN..=u8::MAX {
            let builtin = Builtin::from_u8(raw);
            assert_eq!(
                builtin.and_then(|builtin| Builtin::from_name(builtin.name())),
                builtin
            );
        }
    }
}
