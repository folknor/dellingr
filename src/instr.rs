//! Fixed-width 32-bit instruction encoding.
//!
//! Format: [opcode:8][A:8][B:8][C:8] or [opcode:8][A:8][sBx:16]
//! Each instruction is stored in one `u32`.

/// Argument count for function calls.
///
/// Bytecode supports fixed counts 0-254 or a dynamic count. Host calls can
/// also use `Fixed(255)`; that runtime count cannot be encoded in bytecode.
/// Keeping the runtime count is necessary when `__call` adds a receiver to
/// 254 arguments. Only instruction encoding rejects the sentinel alias.
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
    /// Panics if this is `Fixed(255)`, which aliases `Dynamic` in bytecode.
    #[inline]
    pub(crate) const fn to_u8(self) -> u8 {
        match self {
            ArgCount::Fixed(n) => {
                assert!(
                    n < u8::MAX,
                    "fixed bytecode argument count must be below 255"
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
    /// Panics if this is `Fixed(255)`, which aliases `All` in bytecode.
    #[inline]
    pub(crate) const fn to_u8(self) -> u8 {
        match self {
            RetCount::Fixed(n) => {
                assert!(n < u8::MAX, "fixed bytecode result count must be below 255");
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

/// Static opcode charge categories. Native work is charged separately by its helpers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpcodeCost {
    Free,
    Arithmetic,
    Negation,
    TableCreation,
    TableWrite,
    ArrayElements,
    Call,
}

#[derive(Clone, Copy)]
struct OpcodeMetadata {
    reserved: u32,
    transfer: bool,
    cost: OpcodeCost,
}

macro_rules! define_opcodes {
    ($this:ident, $inst:ident, $f:ident; $( $name:ident = $number:literal, $reserved:literal,
        $transfer:literal, $cost:ident, $debug:expr; )+) => {
        impl Instr {
            $( pub(crate) const $name: u8 = $number; )+

            const OPCODES: [Option<OpcodeMetadata>; 256] = {
                let mut entries = [None; 256];
                $(
                    assert!(entries[$number].is_none());
                    entries[$number] = Some(OpcodeMetadata {
                        reserved: $reserved,
                        transfer: $transfer,
                        cost: OpcodeCost::$cost,
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

// Explicit numbers are snapshot data. Every opcode declares its reserved-bit
// mask, transfer status, static charge and diagnostic operands here. Dynamic
// stack effects and dispatch bodies remain explicit: they encode execution
// semantics, not just operand shape, and must not introduce dispatch lookups.
define_opcodes! { self, this, f;
    OP_POP = 0, 0xffffff, false, Free,
        write!(f, "Pop");
    OP_DUP = 1, 0xffffff, false, Free,
        write!(f, "Dup");
    OP_SWAP = 2, 0xffffff, false, Free,
        write!(f, "Swap");
    OP_NEW_TABLE = 3, 0xffffff, false, TableCreation,
        write!(f, "NewTable");
    OP_GET_TABLE = 4, 0xffffff, false, Free,
        write!(f, "GetTable");
    OP_ADD = 5, 0xffffff, false, Arithmetic,
        write!(f, "Add");
    OP_SUBTRACT = 6, 0xffffff, false, Arithmetic,
        write!(f, "Subtract");
    OP_MULTIPLY = 7, 0xffffff, false, Arithmetic,
        write!(f, "Multiply");
    OP_DIVIDE = 8, 0xffffff, false, Arithmetic,
        write!(f, "Divide");
    OP_POW = 9, 0xffffff, false, Arithmetic,
        write!(f, "Pow");
    OP_MOD = 10, 0xffffff, false, Arithmetic,
        write!(f, "Mod");
    OP_CONCAT = 11, 0x00ffff, false, Free,
        write!(f, "Concat({})", this.a());
    OP_LESS = 12, 0xffffff, false, Free,
        write!(f, "Less");
    OP_LESS_EQUAL = 13, 0xffffff, false, Free,
        write!(f, "LessEqual");
    OP_GREATER = 14, 0xffffff, false, Free,
        write!(f, "Greater");
    OP_GREATER_EQUAL = 15, 0xffffff, false, Free,
        write!(f, "GreaterEqual");
    OP_EQUAL = 16, 0xffffff, false, Free,
        write!(f, "Equal");
    OP_NOT_EQUAL = 17, 0xffffff, false, Free,
        write!(f, "NotEqual");
    OP_NOT = 18, 0xffffff, false, Free,
        write!(f, "Not");
    OP_LENGTH = 19, 0xffffff, false, Free,
        write!(f, "Length");
    OP_NEGATE = 20, 0xffffff, false, Negation,
        write!(f, "Negate");
    OP_MARK_CALL_BASE = 21, 0x00ffff, false, Free,
        write!(f, "MarkCallBase({})", this.a());
    OP_PUSH_NIL = 22, 0xffffff, false, Free,
        write!(f, "PushNil");
    OP_NEW_TABLE_PRESIZED = 23, 0x00ffff, false, TableCreation,
        write!(f, "NewTablePresized({})", this.a());
    OP_NEW_TABLE_TEMPLATE = 24, 0x00ffff, false, TableCreation,
        write!(f, "NewTableTemplate({})", this.a());
    OP_NEW_TABLE_TRACKED = 25, 0x00ffff, false, TableCreation,
        write!(f, "NewTableTracked({})", this.a());
    OP_NOP = 26, 0xffffff, false, Free,
        write!(f, "Nop");
    OP_GET_GLOBAL = 30, 0x000000, false, Free,
        write!(f, "GetGlobal({}, {})", this.a(), this.bx());
    OP_SET_GLOBAL = 31, 0x000000, false, Free,
        write!(f, "SetGlobal({}, {})", this.a(), this.bx());
    OP_GET_LOCAL = 32, 0x00ffff, false, Free,
        write!(f, "GetLocal({})", this.a());
    OP_SET_LOCAL = 33, 0x00ffff, false, Free,
        write!(f, "SetLocal({})", this.a());
    OP_GET_UPVALUE = 34, 0x00ffff, false, Free,
        write!(f, "GetUpvalue({})", this.a());
    OP_SET_UPVALUE = 35, 0x00ffff, false, Free,
        write!(f, "SetUpvalue({})", this.a());
    OP_GET_FIELD = 36, 0x000000, false, Free,
        write!(f, "GetField({}, {})", this.a(), this.bx());
    OP_INIT_INDEX = 37, 0x00ffff, false, TableWrite,
        write!(f, "InitIndex({})", this.a());
    OP_PUSH_NUM = 38, 0xff0000, false, Free,
        write!(f, "PushNum({})", this.bx());
    OP_PUSH_STRING = 39, 0xff0000, false, Free,
        write!(f, "PushString({})", this.bx());
    OP_TFOR_PREP = 40, 0x00ffff, false, Free,
        write!(f, "TForPrep({})", this.a());
    OP_RETURN = 41, 0x00ffff, false, Free,
        write!(f, "Return({:?})", RetCount::from_u8(this.a()));
    OP_CLOSE_UPVALUES = 42, 0x00ffff, false, Free,
        write!(f, "CloseUpvalues({})", this.a());
    OP_CLOSURE = 43, 0x00ffff, false, Free,
        write!(f, "Closure({})", this.a());
    OP_VARARG = 44, 0x00ffff, false, Free,
        write!(f, "Vararg({})", this.a());
    OP_SET_LIST = 45, 0x000000, false, ArrayElements,
        write!(f, "SetList({}, {})", this.a(), this.bx());
    OP_SET_TABLE = 46, 0x00ffff, false, TableWrite,
        write!(f, "SetTable({})", this.a());
    OP_PUSH_BOOL = 47, 0x00ffff, false, Free,
        write!(f, "PushBool({})", this.a() != 0);
    OP_GET_BUILTIN = 48, 0x00ffff, false, Free,
        write!(f, "GetBuiltin({:?})", Builtin::from_u8(this.a()));
    OP_SET_BUILTIN = 49, 0x00ffff, false, Free,
        write!(f, "SetBuiltin({:?})", Builtin::from_u8(this.a()));
    OP_SET_FIELD = 50, 0x000000, false, TableWrite,
        write!(f, "SetField({}, {})", this.a(), this.bx());
    OP_INIT_FIELD = 51, 0x000000, false, TableWrite,
        write!(f, "InitField({}, {})", this.a(), this.bx());
    OP_TFOR_CALL = 52, 0x000000, false, Free,
        write!(f, "TForCall({}, {}, {})", this.a(), this.b(), this.c());
    OP_CALL = 53, 0x0000ff, false, Call,
        write!(f, "Call({:?}, {:?})",
            ArgCount::from_u8(this.a()), RetCount::from_u8(this.b()));
    OP_INIT_FIELD_PINNED = 54, 0x000000, false, TableWrite,
        write!(f, "InitFieldPinned({}, {})", this.a(), this.bx());
    OP_SET_FIELD_AT = 55, 0x000000, false, TableWrite,
        write!(f, "SetFieldAt({}, {})", this.a(), this.bx());
    OP_JUMP = 60, 0xff0000, true, Free,
        write!(f, "Jump({})", this.sbx());
    OP_BRANCH_FALSE = 61, 0xff0000, true, Free,
        write!(f, "BranchFalse({})", this.sbx());
    OP_BRANCH_TRUE_KEEP = 62, 0xff0000, true, Free,
        write!(f, "BranchTrueKeep({})", this.sbx());
    OP_BRANCH_FALSE_KEEP = 63, 0xff0000, true, Free,
        write!(f, "BranchFalseKeep({})", this.sbx());
    OP_BRANCH_FALSE_LESS = 64, 0xff0000, true, Free,
        write!(f, "BranchFalseLess({})", this.sbx());
    OP_BRANCH_FALSE_LESS_EQUAL = 65, 0xff0000, true, Free,
        write!(f, "BranchFalseLessEqual({})", this.sbx());
    OP_BRANCH_FALSE_GREATER = 66, 0xff0000, true, Free,
        write!(f, "BranchFalseGreater({})", this.sbx());
    OP_BRANCH_FALSE_GREATER_EQUAL = 67, 0xff0000, true, Free,
        write!(f, "BranchFalseGreaterEqual({})", this.sbx());
    OP_BRANCH_FALSE_EQUAL = 68, 0xff0000, true, Free,
        write!(f, "BranchFalseEqual({})", this.sbx());
    OP_BRANCH_FALSE_NOT_EQUAL = 69, 0xff0000, true, Free,
        write!(f, "BranchFalseNotEqual({})", this.sbx());
    OP_FOR_PREP = 70, 0x000000, true, Free,
        write!(f, "ForPrep({}, {})", this.a(), this.sbx());
    OP_FOR_LOOP = 71, 0x000000, true, Free,
        write!(f, "ForLoop({}, {})", this.a(), this.sbx());
    OP_TFOR_LOOP = 72, 0x000000, true, Free,
        write!(f, "TForLoop({}, {})", this.a(), this.sbx());
}

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

    /// Create an instruction with two u8 operands (A, B).
    #[inline]
    pub(crate) const fn op_ab(opcode: u8, a: u8, b: u8) -> Self {
        Instr((opcode as u32) << 24 | (a as u32) << 16 | (b as u32) << 8)
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

    // Cache operands are serialized as raw instruction words. Changing the
    // opcode-specific sentinel encodings requires a snapshot format version
    // bump and coordinated updates to the writer, loader, verifier, and VM.
    // Literal-bearing forms: the pool id is a u16 in Bx, and A carries the
    // inline-cache slot (255 = uncached), a stack offset, or is reserved as 0.
    pub(crate) const fn get_global(idx: u16) -> Self {
        Self::op_a_bx(Self::OP_GET_GLOBAL, u8::MAX, idx)
    }
    pub(crate) const fn get_global_cached(idx: u16, cache_idx: u8) -> Self {
        Self::op_a_bx(Self::OP_GET_GLOBAL, cache_idx, idx)
    }
    pub(crate) const fn set_global(idx: u16) -> Self {
        Self::op_a_bx(Self::OP_SET_GLOBAL, 0, idx)
    }
    /// `SET_GLOBAL` uses a biased cache encoding: zero is uncached, while one through 255 name cache slots zero
    /// through 254. This differs from `GET_GLOBAL`'s 255 sentinel.
    pub(crate) const fn set_global_cached(idx: u16, cache_idx: u8) -> Self {
        Self::op_a_bx(Self::OP_SET_GLOBAL, cache_idx + 1, idx)
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
    pub(crate) const fn get_field(idx: u16) -> Self {
        Self::op_a_bx(Self::OP_GET_FIELD, u8::MAX, idx)
    }
    pub(crate) const fn get_field_cached(idx: u16, cache_idx: u8) -> Self {
        Self::op_a_bx(Self::OP_GET_FIELD, cache_idx, idx)
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
    pub(crate) const fn ret(n: RetCount) -> Self {
        Self::op_a(Self::OP_RETURN, n.to_u8())
    }
    pub(crate) const fn close_upvalues(level: u8) -> Self {
        Self::op_a(Self::OP_CLOSE_UPVALUES, level)
    }
    pub(crate) const fn closure(idx: u8) -> Self {
        Self::op_a(Self::OP_CLOSURE, idx)
    }
    pub(crate) const fn vararg(n: u8) -> Self {
        Self::op_a(Self::OP_VARARG, n)
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

    // Field writes: `set_field_at` carries an explicit receiver offset and is
    // never cached; `set_field_cached` implies offset 0 and uses A for its
    // cache slot. Finalization rewrites offset-zero sites to the cached form.
    pub(crate) const fn set_field_at(offset: u8, idx: u16) -> Self {
        Self::op_a_bx(Self::OP_SET_FIELD_AT, offset, idx)
    }
    pub(crate) const fn set_field_cached(idx: u16, cache_idx: u8) -> Self {
        Self::op_a_bx(Self::OP_SET_FIELD, cache_idx, idx)
    }
    pub(crate) const fn init_field(offset: u8, idx: u16) -> Self {
        Self::op_a_bx(Self::OP_INIT_FIELD, offset, idx)
    }
    pub(crate) const fn init_field_pinned(key_idx: u16, entry_idx: u8) -> Self {
        Self::op_a_bx(Self::OP_INIT_FIELD_PINNED, entry_idx, key_idx)
    }
    pub(crate) const fn tfor_call(slot: u8, num_vars: u8) -> Self {
        Self::op_ab(Self::OP_TFOR_CALL, slot, num_vars)
    }
    /// `TFOR_CALL` uses a biased cursor encoding: zero is uncached, while one through 255 are cursor slots
    /// zero through 254.
    pub(crate) const fn tfor_call_cached(slot: u8, num_vars: u8, cache_idx: u8) -> Self {
        Self::op_abc(Self::OP_TFOR_CALL, slot, num_vars, cache_idx + 1)
    }
    pub(crate) const fn call(num_args: ArgCount, num_rets: RetCount) -> Self {
        Self::op_ab(Self::OP_CALL, num_args.to_u8(), num_rets.to_u8())
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
    // including holes and builtin slots, rather than blessing reordered data.
    #[test]
    fn persisted_encodings_match_fixture() {
        use std::fmt::Write;

        let golden = include_str!("../tests/fixtures/opcode_encoding.txt");
        let (version, expected) = golden.split_once('\n').expect("fixture has a version line");
        assert_eq!(version, "format=7");
        let mut actual = String::new();
        for (opcode, name) in Instr::OPCODE_NAMES {
            writeln!(actual, "{name}={opcode}").expect("writing into String cannot fail");
        }
        for (builtin, name) in Builtin::ALL {
            writeln!(actual, "builtin:{name}={}", builtin.to_u8())
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
    #[should_panic(expected = "fixed bytecode argument count must be below 255")]
    fn fixed_sentinel_argument_cannot_be_encoded() {
        let _ = Instr::call(ArgCount::Fixed(255), RetCount::Fixed(0));
    }

    #[test]
    #[should_panic(expected = "fixed bytecode result count must be below 255")]
    fn fixed_sentinel_result_cannot_be_encoded() {
        let _ = Instr::ret(RetCount::Fixed(255));
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
