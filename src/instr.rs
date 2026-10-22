//! Fixed-width 32-bit instruction encoding.
//!
//! Format: [opcode:8][A:8][B:8][C:8] or [opcode:8][A:8][sBx:16]
//! Each instruction is stored in one `u32`.

use crate::limits::{MAX_FIXED_ARGS, MAX_FIXED_RESULTS};
use crate::numeral::{lua_add, lua_div, lua_modulo, lua_mul, lua_sub};

// The fixed-count limits are exactly the counts below the shared 255 sentinel.
const _: () = assert!(MAX_FIXED_ARGS == u8::MAX as usize - 1);
const _: () = assert!(MAX_FIXED_RESULTS == u8::MAX as usize - 1);

/// Inline-cache slots available to one cache family of one function. Cache
/// operands are biased by one: operand 0 means uncached and operands 1..=255
/// name slots 0..=254, so a family holds at most this many slots and its
/// declared count (a `u8`) can never name an unencodable slot.
pub(crate) const MAX_CACHE_SLOTS: usize = u8::MAX as usize;

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
/// Bytecode supports fixed counts 0 through `limits::MAX_FIXED_ARGS` or a
/// dynamic count. Host calls can also use `Fixed(255)`; that runtime count
/// cannot be encoded in bytecode.
/// Keeping the runtime count is necessary when `__call` adds a receiver to
/// 254 arguments. Only instruction encoding rejects the sentinel alias.
//
// A separate host-facing argument type (no `Dynamic`, no sentinel) was
// considered and declined. `Fixed(255)` is not a live collision: runtime
// counts never pass through `to_u8`, which asserts against the alias, and the
// `__call` path needs runtime 255. Hosts naming `Dynamic` get a clean error
// from `call`/`call_anchor` before any state changes. A new type would only
// move that check to compile time, at the cost of breaking every host call
// site; revisit if the host API is redesigned for 1.0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgCount {
    /// Fixed number of arguments (0-254 in bytecode, 0-255 at runtime).
    Fixed(u8),
    /// Bytecode-only: calculate from the pending call-base stack.
    /// Public host calls reject this variant; use [`ArgCount::Fixed`] instead.
    Dynamic,
}

impl ArgCount {
    /// Encode to u8 for instruction storage. Dynamic maps to 255.
    ///
    /// # Panics
    ///
    /// Panics if this is a fixed count above `limits::MAX_FIXED_ARGS`, which
    /// would alias `Dynamic` in bytecode.
    #[inline]
    pub(crate) fn to_u8(self) -> u8 {
        match self {
            ArgCount::Fixed(n) => {
                assert!(
                    usize::from(n) <= MAX_FIXED_ARGS,
                    "fixed bytecode argument count must be at most {MAX_FIXED_ARGS}"
                );
                n
            }
            ArgCount::Dynamic => u8::MAX,
        }
    }

    /// Decode from u8. 255 means Dynamic.
    #[inline]
    pub(crate) const fn from_u8(n: u8) -> Self {
        if n == u8::MAX {
            ArgCount::Dynamic
        } else {
            ArgCount::Fixed(n)
        }
    }
}

/// Return count for function calls and returns.
///
/// Bytecode supports fixed counts 0-254 or all available values. Host calls
/// can also request `Fixed(255)`; that count cannot be encoded in bytecode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetCount {
    /// Fixed number of return values (0-254 in bytecode, 0-255 at runtime).
    Fixed(u8),
    /// Return/expect all available values.
    All,
}

impl RetCount {
    /// Encode to u8 for instruction storage. All maps to 255.
    ///
    /// # Panics
    ///
    /// Panics if this is a fixed count above `limits::MAX_FIXED_RESULTS`,
    /// which would alias `All` in bytecode.
    #[inline]
    pub(crate) fn to_u8(self) -> u8 {
        match self {
            RetCount::Fixed(n) => {
                assert!(
                    usize::from(n) <= MAX_FIXED_RESULTS,
                    "fixed bytecode result count must be at most {MAX_FIXED_RESULTS}"
                );
                n
            }
            RetCount::All => u8::MAX,
        }
    }

    /// Decode from u8. 255 means All.
    #[inline]
    pub(crate) const fn from_u8(n: u8) -> Self {
        if n == u8::MAX {
            RetCount::All
        } else {
            RetCount::Fixed(n)
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

/// A 32-bit encoded instruction.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub(crate) struct Instr(u32);

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

#[derive(Clone, Copy)]
struct OpcodeMetadata {
    reserved: u32,
    transfer: bool,
    cost: OpcodeCost,
    arithmetic: Option<fn(f64, f64) -> f64>,
    foldable: bool,
}

macro_rules! define_opcodes {
    (
        $this:ident, $inst:ident, $f:ident;
        $(
            $name:ident = $number:literal, $reserved:literal,
            $transfer:literal, $cost:ident
                $([$arithmetic:path, $foldable:literal])?,
            $debug:expr;
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
                        reserved: $reserved,
                        transfer: $transfer,
                        cost: OpcodeCost::$cost,
                        arithmetic,
                        foldable,
                    });
                )+
                entries
            };

            pub(crate) const fn known_opcode(opcode: u8) -> bool {
                Self::OPCODES[opcode as usize].is_some()
            }

            pub(crate) const fn reserved_bytes_zero(self) -> bool {
                match Self::OPCODES[self.opcode() as usize] {
                    Some(metadata) => self.0 & metadata.reserved == 0,
                    None => false,
                }
            }

            pub(crate) const fn is_transfer(opcode: u8) -> bool {
                match Self::OPCODES[opcode as usize] {
                    Some(metadata) => metadata.transfer,
                    None => false,
                }
            }

            pub(crate) const fn cost_class(opcode: u8) -> OpcodeCost {
                match Self::OPCODES[opcode as usize] {
                    Some(metadata) => metadata.cost,
                    None => OpcodeCost::Free,
                }
            }

            pub(crate) fn arithmetic_fn(opcode: u8) -> Option<fn(f64, f64) -> f64> {
                match Self::OPCODES[opcode as usize] {
                    Some(metadata) if metadata.foldable => metadata.arithmetic,
                    _ => None,
                }
            }

            /// Fixed opcode units, or the encoded list length for static analysis.
            /// A dynamic SET_LIST has A=0: analysis counts zero, execution uses
            /// the live list length instead. Native charges are not included.
            pub(crate) const fn static_units(self) -> u64 {
                match Self::cost_class(self.opcode()) {
                    OpcodeCost::Arithmetic | OpcodeCost::Negation
                    | OpcodeCost::TableCreation | OpcodeCost::TableWrite => 1,
                    OpcodeCost::ArrayElements => self.a() as u64,
                    OpcodeCost::Free | OpcodeCost::Call => 0,
                }
            }

            #[cfg(test)]
            const OPCODE_NAMES: &'static [(u8, &'static str)] = &[
                $( (Self::$name, stringify!($name)), )+
            ];
        }

        impl std::fmt::Debug for Instr {
            fn fmt(&$this, $f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let $inst = $this;
                match $inst.opcode() {
                    $( Self::$name => $debug, )+
                    op => write!($f, "Unknown(op={}, raw={:#x})", op, $inst.0),
                }
            }
        }
    };
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

// Explicit numbers are snapshot data. Every opcode declares its reserved-bit
// mask, transfer status, static charge and diagnostic operands here. The
// arithmetic function is also declared here so the folder and direct runtime
// dispatch share one mapping. Dynamic stack effects remain explicit.

macro_rules! opcode_rows {
    ($callback:ident, ($($prefix:tt)*), $inst:ident, $f:ident) => {
        $callback! {
            $($prefix)*;
            OP_POP = 0, 0xffffff, false, Free,
                write!($f, "Pop");
            OP_DUP = 1, 0xffffff, false, Free,
                write!($f, "Dup");
            OP_SWAP = 2, 0xffffff, false, Free,
                write!($f, "Swap");
            OP_NEW_TABLE = 3, 0xffffff, false, TableCreation,
                write!($f, "NewTable");
            OP_GET_TABLE = 4, 0xffffff, false, Free,
                write!($f, "GetTable");
            OP_ADD = 5, 0xffffff, false, Arithmetic [lua_add, true],
                write!($f, "Add");
            OP_SUBTRACT = 6, 0xffffff, false, Arithmetic [lua_sub, true],
                write!($f, "Subtract");
            OP_MULTIPLY = 7, 0xffffff, false, Arithmetic [lua_mul, true],
                write!($f, "Multiply");
            OP_DIVIDE = 8, 0xffffff, false, Arithmetic [lua_div, true],
                write!($f, "Divide");
            OP_POW = 9, 0xffffff, false, Arithmetic [crate::crmath::pow, false],
                write!($f, "Pow");
            OP_MOD = 10, 0xffffff, false, Arithmetic [lua_modulo, true],
                write!($f, "Mod");
            OP_CONCAT = 11, 0x00ffff, false, Free,
                write!($f, "Concat({})", $inst.a());
            OP_LESS = 12, 0xffffff, false, Free,
                write!($f, "Less");
            OP_LESS_EQUAL = 13, 0xffffff, false, Free,
                write!($f, "LessEqual");
            OP_GREATER = 14, 0xffffff, false, Free,
                write!($f, "Greater");
            OP_GREATER_EQUAL = 15, 0xffffff, false, Free,
                write!($f, "GreaterEqual");
            OP_EQUAL = 16, 0xffffff, false, Free,
                write!($f, "Equal");
            OP_NOT_EQUAL = 17, 0xffffff, false, Free,
                write!($f, "NotEqual");
            OP_NOT = 18, 0xffffff, false, Free,
                write!($f, "Not");
            OP_LENGTH = 19, 0xffffff, false, Free,
                write!($f, "Length");
            OP_NEGATE = 20, 0xffffff, false, Negation,
                write!($f, "Negate");
            OP_MARK_CALL_BASE = 21, 0x00ffff, false, Free,
                write!($f, "MarkCallBase({})", $inst.a());
            OP_PUSH_NIL = 22, 0xffffff, false, Free,
                write!($f, "PushNil");
            OP_NEW_TABLE_PRESIZED = 23, 0x00ffff, false, TableCreation,
                write!($f, "NewTablePresized({})", $inst.a());
            OP_NEW_TABLE_TEMPLATE = 24, 0x00ffff, false, TableCreation,
                write!($f, "NewTableTemplate({})", $inst.a());
            OP_NEW_TABLE_TRACKED = 25, 0x00ffff, false, TableCreation,
                write!($f, "NewTableTracked({})", $inst.a());
            OP_NOP = 26, 0xffffff, false, Free,
                write!($f, "Nop");
            OP_GET_GLOBAL = 30, 0x000000, false, Free,
                write!($f, "GetGlobal({}, {:?})", $inst.bx(), $inst.global_cache_slot());
            OP_SET_GLOBAL = 31, 0x000000, false, Free,
                write!($f, "SetGlobal({}, {:?})", $inst.bx(), $inst.global_cache_slot());
            OP_GET_LOCAL = 32, 0x00ffff, false, Free,
                write!($f, "GetLocal({})", $inst.a());
            OP_SET_LOCAL = 33, 0x00ffff, false, Free,
                write!($f, "SetLocal({})", $inst.a());
            OP_GET_UPVALUE = 34, 0x00ffff, false, Free,
                write!($f, "GetUpvalue({})", $inst.a());
            OP_SET_UPVALUE = 35, 0x00ffff, false, Free,
                write!($f, "SetUpvalue({})", $inst.a());
            OP_GET_FIELD = 36, 0x000000, false, Free,
                write!($f, "GetField({}, {:?})", $inst.bx(), $inst.field_cache_slot());
            OP_INIT_INDEX = 37, 0x00ffff, false, TableWrite,
                write!($f, "InitIndex({})", $inst.a());
            OP_PUSH_NUM = 38, 0xff0000, false, Free,
                write!($f, "PushNum({})", $inst.bx());
            OP_PUSH_STRING = 39, 0xff0000, false, Free,
                write!($f, "PushString({})", $inst.bx());
            OP_TFOR_PREP = 40, 0x00ffff, false, Free,
                write!($f, "TForPrep({})", $inst.a());
            OP_RETURN = 41, 0x00ffff, false, Free,
                write!($f, "Return({:?})", $inst.return_count());
            OP_CLOSE_UPVALUES = 42, 0x00ffff, false, Free,
                write!($f, "CloseUpvalues({})", $inst.a());
            OP_CLOSURE = 43, 0x00ffff, false, Free,
                write!($f, "Closure({})", $inst.a());
            OP_VARARG = 44, 0x00ffff, false, Free,
                write!($f, "Vararg({:?})", $inst.vararg_count());
            OP_SET_LIST = 45, 0x000000, false, ArrayElements,
                write!($f, "SetList({}, {})", $inst.a(), $inst.bx());
            OP_SET_TABLE = 46, 0x00ffff, false, TableWrite,
                write!($f, "SetTable({})", $inst.a());
            OP_PUSH_BOOL = 47, 0x00ffff, false, Free,
                write!($f, "PushBool({})", $inst.a() != 0);
            OP_GET_BUILTIN = 48, 0x00ffff, false, Free,
                write!($f, "GetBuiltin({:?})", Builtin::from_u8($inst.a()));
            OP_SET_BUILTIN = 49, 0x00ffff, false, Free,
                write!($f, "SetBuiltin({:?})", Builtin::from_u8($inst.a()));
            OP_SET_FIELD = 50, 0x000000, false, TableWrite,
                write!($f, "SetField({}, {:?})", $inst.bx(), $inst.set_field_cache_slot());
            OP_INIT_FIELD = 51, 0x000000, false, TableWrite,
                write!($f, "InitField({}, {})", $inst.a(), $inst.bx());
            OP_TFOR_CALL = 52, 0x000000, false, Free,
                write!($f, "TForCall({}, {}, {:?})",
                    $inst.a(), $inst.b(), $inst.tfor_cursor_slot());
            OP_CALL = 53, 0x0000fe, false, Call,
                write!($f, "Call({:?}, {:?})", $inst.call_args(), $inst.call_results());
            OP_INIT_FIELD_PINNED = 54, 0x000000, false, TableWrite,
                write!($f, "InitFieldPinned({}, {})", $inst.a(), $inst.bx());
            OP_SET_FIELD_AT = 55, 0x000000, false, TableWrite,
                write!($f, "SetFieldAt({}, {})", $inst.a(), $inst.bx());
            OP_JUMP = 60, 0xff0000, true, Free,
                write!($f, "Jump({})", $inst.sbx());
            OP_BRANCH_FALSE = 61, 0xff0000, true, Free,
                write!($f, "BranchFalse({})", $inst.sbx());
            OP_BRANCH_TRUE_KEEP = 62, 0xff0000, true, Free,
                write!($f, "BranchTrueKeep({})", $inst.sbx());
            OP_BRANCH_FALSE_KEEP = 63, 0xff0000, true, Free,
                write!($f, "BranchFalseKeep({})", $inst.sbx());
            OP_BRANCH_FALSE_LESS = 64, 0xff0000, true, Free,
                write!($f, "BranchFalseLess({})", $inst.sbx());
            OP_BRANCH_FALSE_LESS_EQUAL = 65, 0xff0000, true, Free,
                write!($f, "BranchFalseLessEqual({})", $inst.sbx());
            OP_BRANCH_FALSE_GREATER = 66, 0xff0000, true, Free,
                write!($f, "BranchFalseGreater({})", $inst.sbx());
            OP_BRANCH_FALSE_GREATER_EQUAL = 67, 0xff0000, true, Free,
                write!($f, "BranchFalseGreaterEqual({})", $inst.sbx());
            OP_BRANCH_FALSE_EQUAL = 68, 0xff0000, true, Free,
                write!($f, "BranchFalseEqual({})", $inst.sbx());
            OP_BRANCH_FALSE_NOT_EQUAL = 69, 0xff0000, true, Free,
                write!($f, "BranchFalseNotEqual({})", $inst.sbx());
            OP_FOR_PREP = 70, 0x000000, true, Free,
                write!($f, "ForPrep({}, {})", $inst.a(), $inst.sbx());
            OP_FOR_LOOP = 71, 0x000000, true, Free,
                write!($f, "ForLoop({}, {})", $inst.a(), $inst.sbx());
            OP_TFOR_LOOP = 72, 0x000000, true, Free,
                write!($f, "TForLoop({}, {})", $inst.a(), $inst.sbx());
        }
    };
}
pub(crate) use opcode_rows;

opcode_rows!(define_opcodes, (self, this, f), this, f);

macro_rules! define_comparisons {
    ($( $plain:ident => $fused:ident, $constructor:ident
        $(, $rule:ident = ($ordering:ident, $negate:literal))?; )+) => {
        impl Instr {
            $( $(pub(crate) const $rule: (std::cmp::Ordering, bool) =
                (std::cmp::Ordering::$ordering, $negate);)? )+

            pub(crate) fn fused_branch_constructor(opcode: u8) -> Option<fn(i16) -> Self> {
                match opcode {
                    $( Self::$plain => Some(Self::$constructor), )+
                    _ => None,
                }
            }
        }

        // The dispatch range must contain exactly these variants, with no
        // holes or duplicates. Keep its one-range-arm code generation intact.
        const _: () = {
            let variants = [$( Instr::$fused ),+];
            let first = Instr::OP_BRANCH_FALSE_LESS;
            let last = Instr::OP_BRANCH_FALSE_NOT_EQUAL;
            assert!(variants.len() == (last - first + 1) as usize);
            let mut seen = [false; 256];
            let mut i = 0;
            while i < variants.len() {
                let opcode = variants[i];
                assert!(opcode >= first && opcode <= last);
                assert!(!seen[opcode as usize]);
                seen[opcode as usize] = true;
                i += 1;
            }
        };
    };
}

// Inclusive ordering deliberately negates the opposite strict ordering. Both
// ordinary and fused dispatch consume these constants without a lookup.
define_comparisons! {
    OP_LESS => OP_BRANCH_FALSE_LESS, branch_false_less, LESS_ORDER = (Less, false);
    OP_LESS_EQUAL => OP_BRANCH_FALSE_LESS_EQUAL, branch_false_less_equal, LESS_EQUAL_ORDER = (Greater, true);
    OP_GREATER => OP_BRANCH_FALSE_GREATER, branch_false_greater, GREATER_ORDER = (Greater, false);
    OP_GREATER_EQUAL => OP_BRANCH_FALSE_GREATER_EQUAL, branch_false_greater_equal, GREATER_EQUAL_ORDER = (Less, true);
    OP_EQUAL => OP_BRANCH_FALSE_EQUAL, branch_false_equal;
    OP_NOT_EQUAL => OP_BRANCH_FALSE_NOT_EQUAL, branch_false_not_equal;
}

// Typed constructors remain explicit: boolean conversion, count sentinels,
// biased cache operands and signed offsets are distinct encoding contracts.
// The declaration table owns reserved bits; it must not infer these values.
// Every sentinel-bearing operand has a typed decoder beside its constructor;
// the VM, the verifier, the finalizer and the disassembler read those operands
// only through the decoders, never as raw bytes.
impl Instr {
    /// Create an instruction with no operands.
    #[inline]
    pub(crate) const fn op(opcode: u8) -> Self {
        Instr((opcode as u32) << 24)
    }

    /// Create an instruction with one u8 operand (A).
    #[inline]
    pub(crate) const fn op_a(opcode: u8, a: u8) -> Self {
        Instr((opcode as u32) << 24 | (a as u32) << 16)
    }

    /// Create an instruction with three u8 operands (A, B, C).
    #[inline]
    pub(crate) const fn op_abc(opcode: u8, a: u8, b: u8, c: u8) -> Self {
        Instr((opcode as u32) << 24 | (a as u32) << 16 | (b as u32) << 8 | c as u32)
    }

    /// Create an instruction with a signed 16-bit offset (sBx).
    #[inline]
    pub(crate) const fn op_sbx(opcode: u8, sbx: i16) -> Self {
        Instr((opcode as u32) << 24 | (sbx as u16 as u32))
    }

    /// Create an instruction with one u8 and a signed 16-bit offset (A, sBx).
    #[inline]
    pub(crate) const fn op_a_sbx(opcode: u8, a: u8, sbx: i16) -> Self {
        Instr((opcode as u32) << 24 | (a as u32) << 16 | (sbx as u16 as u32))
    }

    /// Create an instruction with one u8 and one u16 operand (A, Bx).
    #[inline]
    pub(crate) const fn op_a_bx(opcode: u8, a: u8, bx: u16) -> Self {
        Instr((opcode as u32) << 24 | (a as u32) << 16 | bx as u32)
    }

    /// Get the opcode.
    #[inline]
    pub(crate) const fn opcode(self) -> u8 {
        (self.0 >> 24) as u8
    }

    /// Get the A operand.
    #[inline]
    pub(crate) const fn a(self) -> u8 {
        (self.0 >> 16) as u8
    }

    /// Get the B operand.
    #[inline]
    pub(crate) const fn b(self) -> u8 {
        (self.0 >> 8) as u8
    }

    /// Get the C operand.
    #[inline]
    pub(crate) const fn c(self) -> u8 {
        self.0 as u8
    }

    /// Get the unsigned 16-bit Bx operand.
    #[inline]
    pub(crate) const fn bx(self) -> u16 {
        self.0 as u16
    }

    /// Get the signed 16-bit offset (sBx).
    #[inline]
    pub(crate) const fn sbx(self) -> i16 {
        self.0 as u16 as i16
    }

    /// Get the raw u32 value.
    #[inline]
    pub(crate) const fn raw(self) -> u32 {
        self.0
    }

    /// Rebuild an instruction from its raw 32-bit encoding.
    #[inline]
    pub(crate) const fn from_raw(raw: u32) -> Self {
        Instr(raw)
    }
}

// Convenient constructors for each instruction type
impl Instr {
    // No operands
    pub(crate) const fn pop() -> Self {
        Self::op(Self::OP_POP)
    }
    pub(crate) const fn nop() -> Self {
        Self::op(Self::OP_NOP)
    }
    pub(crate) const fn dup() -> Self {
        Self::op(Self::OP_DUP)
    }
    pub(crate) const fn swap() -> Self {
        Self::op(Self::OP_SWAP)
    }
    pub(crate) const fn new_table() -> Self {
        Self::op(Self::OP_NEW_TABLE)
    }
    pub(crate) const fn new_table_presized(capacity: u8) -> Self {
        Self::op_a(Self::OP_NEW_TABLE_PRESIZED, capacity)
    }
    pub(crate) const fn new_table_template(idx: u8) -> Self {
        Self::op_a(Self::OP_NEW_TABLE_TEMPLATE, idx)
    }
    pub(crate) const fn new_table_tracked(capacity: u8) -> Self {
        Self::op_a(Self::OP_NEW_TABLE_TRACKED, capacity)
    }
    pub(crate) const fn get_table() -> Self {
        Self::op(Self::OP_GET_TABLE)
    }
    pub(crate) const fn add() -> Self {
        Self::op(Self::OP_ADD)
    }
    pub(crate) const fn subtract() -> Self {
        Self::op(Self::OP_SUBTRACT)
    }
    pub(crate) const fn multiply() -> Self {
        Self::op(Self::OP_MULTIPLY)
    }
    pub(crate) const fn divide() -> Self {
        Self::op(Self::OP_DIVIDE)
    }
    pub(crate) const fn pow() -> Self {
        Self::op(Self::OP_POW)
    }
    pub(crate) const fn modulo() -> Self {
        Self::op(Self::OP_MOD)
    }
    pub(crate) const fn concat(n: u8) -> Self {
        Self::op_a(Self::OP_CONCAT, n)
    }
    pub(crate) const fn less() -> Self {
        Self::op(Self::OP_LESS)
    }
    pub(crate) const fn less_equal() -> Self {
        Self::op(Self::OP_LESS_EQUAL)
    }
    pub(crate) const fn greater() -> Self {
        Self::op(Self::OP_GREATER)
    }
    pub(crate) const fn greater_equal() -> Self {
        Self::op(Self::OP_GREATER_EQUAL)
    }
    pub(crate) const fn equal() -> Self {
        Self::op(Self::OP_EQUAL)
    }
    pub(crate) const fn not_equal() -> Self {
        Self::op(Self::OP_NOT_EQUAL)
    }
    pub(crate) const fn not() -> Self {
        Self::op(Self::OP_NOT)
    }
    pub(crate) const fn length() -> Self {
        Self::op(Self::OP_LENGTH)
    }
    pub(crate) const fn negate() -> Self {
        Self::op(Self::OP_NEGATE)
    }
    pub(crate) const fn mark_call_base(adjustment: u8) -> Self {
        Self::op_a(Self::OP_MARK_CALL_BASE, adjustment)
    }
    pub(crate) const fn push_nil() -> Self {
        Self::op(Self::OP_PUSH_NIL)
    }

    // Cache operands are serialized as raw instruction words, so changing
    // their encoding requires a snapshot format version bump. Every cache
    // operand (GET_GLOBAL, SET_GLOBAL, GET_FIELD and SET_FIELD in A, TFOR_CALL
    // in C) uses the one biased encoding of `encode_cache_slot`: 0 is uncached.
    // Literal-bearing forms: the pool id is a u16 in Bx, and A carries the
    // inline-cache slot, a stack offset, or is reserved as 0.
    pub(crate) fn get_global(idx: u16, cache_slot: Option<u8>) -> Self {
        Self::op_a_bx(Self::OP_GET_GLOBAL, encode_cache_slot(cache_slot), idx)
    }
    pub(crate) fn set_global(idx: u16, cache_slot: Option<u8>) -> Self {
        Self::op_a_bx(Self::OP_SET_GLOBAL, encode_cache_slot(cache_slot), idx)
    }
    /// The global-lookup cache slot of a `GET_GLOBAL` or `SET_GLOBAL`. Both
    /// opcodes draw from the one global cache of their function.
    #[inline(always)]
    pub(crate) const fn global_cache_slot(self) -> Option<u8> {
        debug_assert!(matches!(
            self.opcode(),
            Self::OP_GET_GLOBAL | Self::OP_SET_GLOBAL
        ));
        decode_cache_slot(self.a())
    }
    pub(crate) const fn get_local(slot: u8) -> Self {
        Self::op_a(Self::OP_GET_LOCAL, slot)
    }
    pub(crate) const fn set_local(slot: u8) -> Self {
        Self::op_a(Self::OP_SET_LOCAL, slot)
    }
    pub(crate) const fn get_upvalue(idx: u8) -> Self {
        Self::op_a(Self::OP_GET_UPVALUE, idx)
    }
    pub(crate) const fn set_upvalue(idx: u8) -> Self {
        Self::op_a(Self::OP_SET_UPVALUE, idx)
    }
    pub(crate) fn get_field(idx: u16, cache_slot: Option<u8>) -> Self {
        Self::op_a_bx(Self::OP_GET_FIELD, encode_cache_slot(cache_slot), idx)
    }
    /// The field-lookup cache slot of a `GET_FIELD`.
    #[inline(always)]
    pub(crate) const fn field_cache_slot(self) -> Option<u8> {
        debug_assert!(self.opcode() == Self::OP_GET_FIELD);
        decode_cache_slot(self.a())
    }
    pub(crate) const fn init_index(offset: u8) -> Self {
        Self::op_a(Self::OP_INIT_INDEX, offset)
    }
    pub(crate) const fn push_num(idx: u16) -> Self {
        Self::op_a_bx(Self::OP_PUSH_NUM, 0, idx)
    }
    pub(crate) const fn push_string(idx: u16) -> Self {
        Self::op_a_bx(Self::OP_PUSH_STRING, 0, idx)
    }
    pub(crate) const fn tfor_prep(slot: u8) -> Self {
        Self::op_a(Self::OP_TFOR_PREP, slot)
    }
    pub(crate) fn ret(n: RetCount) -> Self {
        Self::op_a(Self::OP_RETURN, n.to_u8())
    }
    /// The result count of an `OP_RETURN`.
    #[inline(always)]
    pub(crate) const fn return_count(self) -> RetCount {
        debug_assert!(self.opcode() == Self::OP_RETURN);
        RetCount::from_u8(self.a())
    }
    pub(crate) const fn close_upvalues(level: u8) -> Self {
        Self::op_a(Self::OP_CLOSE_UPVALUES, level)
    }
    pub(crate) const fn closure(idx: u8) -> Self {
        Self::op_a(Self::OP_CLOSURE, idx)
    }
    /// Push `n` varargs padded with nil, or every available vararg for
    /// `RetCount::All`.
    pub(crate) fn vararg(n: RetCount) -> Self {
        Self::op_a(Self::OP_VARARG, n.to_u8())
    }
    /// The value count of an `OP_VARARG`.
    #[inline(always)]
    pub(crate) const fn vararg_count(self) -> RetCount {
        debug_assert!(self.opcode() == Self::OP_VARARG);
        RetCount::from_u8(self.a())
    }
    /// Test-only: production emission always names its batch explicitly through
    /// `set_list_batch`, so this shorthand for batch 0 exists for assertions.
    #[cfg(test)]
    pub(crate) const fn set_list(count: u8) -> Self {
        Self::set_list_batch(count, 0)
    }
    pub(crate) const fn set_list_batch(count: u8, batch: u16) -> Self {
        Self::op_a_bx(Self::OP_SET_LIST, count, batch)
    }
    pub(crate) const fn set_table(offset: u8) -> Self {
        Self::op_a(Self::OP_SET_TABLE, offset)
    }
    pub(crate) const fn push_bool(b: bool) -> Self {
        Self::op_a(Self::OP_PUSH_BOOL, b as u8)
    }
    pub(crate) const fn get_builtin(slot: Builtin) -> Self {
        Self::op_a(Self::OP_GET_BUILTIN, slot.to_u8())
    }
    pub(crate) const fn set_builtin(slot: Builtin) -> Self {
        Self::op_a(Self::OP_SET_BUILTIN, slot.to_u8())
    }

    // Field writes: `set_field_at` carries an explicit receiver offset in A
    // and has no cache operand; `set_field` implies offset 0 and uses A for
    // its cache slot. Finalization rewrites offset-zero sites to `set_field`.
    pub(crate) const fn set_field_at(offset: u8, idx: u16) -> Self {
        Self::op_a_bx(Self::OP_SET_FIELD_AT, offset, idx)
    }
    pub(crate) fn set_field(idx: u16, cache_slot: Option<u8>) -> Self {
        Self::op_a_bx(Self::OP_SET_FIELD, encode_cache_slot(cache_slot), idx)
    }
    /// The set-field cache slot of a `SET_FIELD`.
    #[inline(always)]
    pub(crate) const fn set_field_cache_slot(self) -> Option<u8> {
        debug_assert!(self.opcode() == Self::OP_SET_FIELD);
        decode_cache_slot(self.a())
    }
    pub(crate) const fn init_field(offset: u8, idx: u16) -> Self {
        Self::op_a_bx(Self::OP_INIT_FIELD, offset, idx)
    }
    pub(crate) const fn init_field_pinned(key_idx: u16, entry_idx: u8) -> Self {
        Self::op_a_bx(Self::OP_INIT_FIELD_PINNED, entry_idx, key_idx)
    }
    pub(crate) fn tfor_call(slot: u8, num_vars: u8, cursor_slot: Option<u8>) -> Self {
        Self::op_abc(
            Self::OP_TFOR_CALL,
            slot,
            num_vars,
            encode_cache_slot(cursor_slot),
        )
    }
    /// The `pairs` cursor cache slot of a `TFOR_CALL`.
    #[inline(always)]
    pub(crate) const fn tfor_cursor_slot(self) -> Option<u8> {
        debug_assert!(self.opcode() == Self::OP_TFOR_CALL);
        decode_cache_slot(self.c())
    }
    #[cfg(test)]
    pub(crate) fn call(num_args: ArgCount, num_rets: RetCount) -> Self {
        Self::call_site(num_args, num_rets, false)
    }
    /// An `OP_CALL`, marked when it is the call of a method call (`obj:m()`).
    /// The mark changes nothing at execution; a failing library function reads
    /// it to number its arguments the way the script author wrote them.
    pub(crate) fn call_site(num_args: ArgCount, num_rets: RetCount, method: bool) -> Self {
        Self::op_abc(
            Self::OP_CALL,
            num_args.to_u8(),
            num_rets.to_u8(),
            u8::from(method),
        )
    }
    /// Whether an `OP_CALL` is the call of a method call (`obj:m()`).
    #[inline(always)]
    pub(crate) const fn call_is_method(self) -> bool {
        debug_assert!(self.opcode() == Self::OP_CALL);
        self.c() != 0
    }
    /// The argument count of an `OP_CALL`.
    #[inline(always)]
    pub(crate) const fn call_args(self) -> ArgCount {
        debug_assert!(self.opcode() == Self::OP_CALL);
        ArgCount::from_u8(self.a())
    }
    /// The result count an `OP_CALL` asks for.
    #[inline(always)]
    pub(crate) const fn call_results(self) -> RetCount {
        debug_assert!(self.opcode() == Self::OP_CALL);
        RetCount::from_u8(self.b())
    }

    // Jump instructions (signed 16-bit offset)
    pub(crate) const fn jump(offset: i16) -> Self {
        Self::op_sbx(Self::OP_JUMP, offset)
    }
    pub(crate) const fn branch_false(offset: i16) -> Self {
        Self::op_sbx(Self::OP_BRANCH_FALSE, offset)
    }
    pub(crate) const fn branch_true_keep(offset: i16) -> Self {
        Self::op_sbx(Self::OP_BRANCH_TRUE_KEEP, offset)
    }
    pub(crate) const fn branch_false_keep(offset: i16) -> Self {
        Self::op_sbx(Self::OP_BRANCH_FALSE_KEEP, offset)
    }
    pub(crate) const fn branch_false_less(offset: i16) -> Self {
        Self::op_sbx(Self::OP_BRANCH_FALSE_LESS, offset)
    }
    pub(crate) const fn branch_false_less_equal(offset: i16) -> Self {
        Self::op_sbx(Self::OP_BRANCH_FALSE_LESS_EQUAL, offset)
    }
    pub(crate) const fn branch_false_greater(offset: i16) -> Self {
        Self::op_sbx(Self::OP_BRANCH_FALSE_GREATER, offset)
    }
    pub(crate) const fn branch_false_greater_equal(offset: i16) -> Self {
        Self::op_sbx(Self::OP_BRANCH_FALSE_GREATER_EQUAL, offset)
    }
    pub(crate) const fn branch_false_equal(offset: i16) -> Self {
        Self::op_sbx(Self::OP_BRANCH_FALSE_EQUAL, offset)
    }
    pub(crate) const fn branch_false_not_equal(offset: i16) -> Self {
        Self::op_sbx(Self::OP_BRANCH_FALSE_NOT_EQUAL, offset)
    }

    // u8 + signed 16-bit offset
    pub(crate) const fn for_prep(slot: u8, offset: i16) -> Self {
        Self::op_a_sbx(Self::OP_FOR_PREP, slot, offset)
    }
    pub(crate) const fn for_loop(slot: u8, offset: i16) -> Self {
        Self::op_a_sbx(Self::OP_FOR_LOOP, slot, offset)
    }
    pub(crate) const fn tfor_loop(slot: u8, offset: i16) -> Self {
        Self::op_a_sbx(Self::OP_TFOR_LOOP, slot, offset)
    }
}

#[cfg(test)]
mod tests {
    use super::{ArgCount, Builtin, Instr, RetCount};

    // This fixture is deliberately independent of the declaration tables.
    // Snapshot version changes must review this entire encoding contract,
    // including holes, builtin slots and cache operands, rather than blessing
    // reordered data.
    #[test]
    fn persisted_encodings_match_fixture() {
        use std::fmt::Write;

        let golden = include_str!("../tests/fixtures/opcode_encoding.txt");
        let (version, expected) = golden.split_once('\n').expect("fixture has a version line");
        assert_eq!(
            version,
            format!("format={}", super::snapshot_format::FORMAT_VERSION)
        );
        let mut actual = String::new();
        for (opcode, name) in Instr::OPCODE_NAMES {
            writeln!(actual, "{name}={opcode}").expect("writing into String cannot fail");
        }
        for (builtin, name) in Builtin::ALL {
            writeln!(actual, "builtin:{name}={}", builtin.to_u8())
                .expect("writing into String cannot fail");
        }
        // Cache operands are persisted inside raw instruction words: pin the
        // uncached, first and last slot encodings of every cache family.
        type CacheFamily = (&'static str, fn(Option<u8>) -> Instr);
        let cache_families: [CacheFamily; 5] = [
            ("OP_GET_GLOBAL", |s| Instr::get_global(7, s)),
            ("OP_SET_GLOBAL", |s| Instr::set_global(7, s)),
            ("OP_GET_FIELD", |s| Instr::get_field(7, s)),
            ("OP_SET_FIELD", |s| Instr::set_field(7, s)),
            ("OP_TFOR_CALL", |s| Instr::tfor_call(3, 2, s)),
        ];
        for (name, encode) in cache_families {
            writeln!(
                actual,
                "cache:{name}={:08x},{:08x},{:08x}",
                encode(None).raw(),
                encode(Some(0)).raw(),
                encode(Some(254)).raw()
            )
            .expect("writing into String cannot fail");
        }
        assert_eq!(
            actual, expected,
            "snapshot opcode or builtin encoding changed"
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
                "review the encoding fixture when changing the snapshot format"
            );
        }
    }

    #[test]
    fn count_encoding_round_trips_without_fixed_sentinel_aliases() {
        for n in 0..u8::MAX {
            assert_eq!(
                ArgCount::from_u8(ArgCount::Fixed(n).to_u8()),
                ArgCount::Fixed(n)
            );
            assert_eq!(
                RetCount::from_u8(RetCount::Fixed(n).to_u8()),
                RetCount::Fixed(n)
            );
        }
        assert_eq!(ArgCount::from_u8(u8::MAX), ArgCount::Dynamic);
        assert_eq!(RetCount::from_u8(u8::MAX), RetCount::All);
    }

    #[test]
    fn arithmetic_functions_come_from_opcode_rows() {
        type ArithFn = fn(f64, f64) -> f64;
        let expected: [(u8, ArithFn); 5] = [
            (Instr::OP_ADD, crate::numeral::lua_add),
            (Instr::OP_SUBTRACT, crate::numeral::lua_sub),
            (Instr::OP_MULTIPLY, crate::numeral::lua_mul),
            (Instr::OP_DIVIDE, crate::numeral::lua_div),
            (Instr::OP_MOD, crate::numeral::lua_modulo),
        ];
        for (opcode, expected_fn) in expected {
            let actual_fn = Instr::arithmetic_fn(opcode)
                .expect("foldable arithmetic opcode declares a function");
            assert_eq!(
                actual_fn(4.5, -2.0).to_bits(),
                expected_fn(4.5, -2.0).to_bits()
            );
        }
        assert!(Instr::arithmetic_fn(Instr::OP_POW).is_none());
        assert!(Instr::arithmetic_fn(Instr::OP_NEGATE).is_none());
    }

    #[test]
    #[should_panic(expected = "fixed bytecode argument count must be at most 254")]
    fn fixed_sentinel_argument_cannot_be_encoded() {
        let _ = Instr::call(ArgCount::Fixed(255), RetCount::Fixed(0));
    }

    #[test]
    #[should_panic(expected = "fixed bytecode result count must be at most 254")]
    fn fixed_sentinel_result_cannot_be_encoded() {
        let _ = Instr::ret(RetCount::Fixed(255));
    }

    #[test]
    #[should_panic(expected = "fixed bytecode result count must be at most 254")]
    fn fixed_sentinel_vararg_cannot_be_encoded() {
        let _ = Instr::vararg(RetCount::Fixed(255));
    }

    #[test]
    #[should_panic(expected = "cache slot must be below 255")]
    fn cache_slot_255_cannot_be_encoded() {
        let _ = Instr::get_field(0, Some(u8::MAX));
    }

    // Snapshot data: every cache operand is biased by one with 0 = uncached,
    // so the largest slot (254) encodes as 255 and no slot aliases "uncached".
    #[test]
    fn cache_operands_share_one_biased_encoding() {
        type Family = (
            fn(Option<u8>) -> Instr,
            fn(Instr) -> u8,
            fn(Instr) -> Option<u8>,
        );
        let families: [Family; 5] = [
            (
                |s| Instr::get_global(7, s),
                Instr::a,
                Instr::global_cache_slot,
            ),
            (
                |s| Instr::set_global(7, s),
                Instr::a,
                Instr::global_cache_slot,
            ),
            (
                |s| Instr::get_field(7, s),
                Instr::a,
                Instr::field_cache_slot,
            ),
            (
                |s| Instr::set_field(7, s),
                Instr::a,
                Instr::set_field_cache_slot,
            ),
            (
                |s| Instr::tfor_call(3, 2, s),
                Instr::c,
                Instr::tfor_cursor_slot,
            ),
        ];
        assert_eq!(super::MAX_CACHE_SLOTS, 255);
        for (encode, operand, decode) in families {
            assert_eq!(operand(encode(None)), 0);
            assert_eq!(decode(encode(None)), None);
            for slot in 0..u8::MAX {
                let inst = encode(Some(slot));
                assert_eq!(operand(inst), slot + 1);
                assert_eq!(decode(inst), Some(slot));
            }
        }
        // The operands that are not the cache are untouched by the encoding.
        assert_eq!(Instr::get_global(7, Some(3)).bx(), 7);
        let tfor = Instr::tfor_call(3, 2, Some(9));
        assert_eq!((tfor.a(), tfor.b()), (3, 2));
    }

    #[test]
    fn count_operands_decode_through_their_count_types() {
        let call = Instr::call(ArgCount::Dynamic, RetCount::All);
        assert_eq!(call.call_args(), ArgCount::Dynamic);
        assert_eq!(call.call_results(), RetCount::All);
        let call = Instr::call(ArgCount::Fixed(254), RetCount::Fixed(0));
        assert_eq!(call.call_args(), ArgCount::Fixed(254));
        assert_eq!(call.call_results(), RetCount::Fixed(0));
        assert_eq!(Instr::ret(RetCount::All).return_count(), RetCount::All);
        assert_eq!(
            Instr::ret(RetCount::Fixed(3)).return_count(),
            RetCount::Fixed(3)
        );
        assert_eq!(Instr::vararg(RetCount::All).vararg_count(), RetCount::All);
        assert_eq!(
            Instr::vararg(RetCount::Fixed(254)).vararg_count(),
            RetCount::Fixed(254)
        );
    }

    #[test]
    fn diagnostic_operands_include_counts_and_marker_adjustments() {
        assert_eq!(
            format!("{:?}", Instr::op_a(Instr::OP_CONCAT, 3)),
            "Concat(3)"
        );
        assert_eq!(format!("{:?}", Instr::mark_call_base(1)), "MarkCallBase(1)");
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
