//! Fixed-width 32-bit instruction encoding.
//!
//! Format: [opcode:8][A:8][B:8][C:8] or [opcode:8][A:8][sBx:16]
//!
//! This encoding is 4x smaller than the previous enum-based representation
//! (~16 bytes per instruction) and much more cache-friendly.

/// Argument count for function calls.
///
/// Either a fixed count (0-254) or dynamic (calculated from vararg call base).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgCount {
    /// Fixed number of arguments (0-254).
    Fixed(u8),
    /// Dynamic: calculate from vararg_call_bases stack.
    Dynamic,
}

impl ArgCount {
    /// Encode to u8 for instruction storage. Dynamic maps to 255.
    #[inline]
    pub(crate) const fn to_u8(self) -> u8 {
        match self {
            ArgCount::Fixed(n) => n,
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
/// Either a fixed count (0-254) or all available values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetCount {
    /// Fixed number of return values (0-254).
    Fixed(u8),
    /// Return/expect all available values.
    All,
}

impl RetCount {
    /// Encode to u8 for instruction storage. All maps to 255.
    #[inline]
    pub(crate) const fn to_u8(self) -> u8 {
        match self {
            RetCount::Fixed(n) => n,
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

/// Well-known global names that get fast array-indexed access instead of HashMap lookup.
///
/// These are the most commonly accessed globals in typical Lua scripts.
/// Using a fixed array eliminates string hashing and comparison overhead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Builtin {
    // Common functions
    Print = 0,
    Type = 1,
    Tonumber = 2,
    Tostring = 3,
    // Iteration
    Pairs = 4,
    Ipairs = 5,
    Next = 6,
    // Table operations
    Getmetatable = 7,
    Setmetatable = 8,
    Rawget = 9,
    Rawset = 10,
    Rawequal = 11,
    Rawlen = 12,
    // Vararg helpers
    Select = 13,
    Unpack = 14,
    // Library tables
    Math = 15,
    String = 16,
    Table = 17,
    // Global environment
    G = 18, // _G
}

impl Builtin {
    /// Total number of builtin slots.
    pub const COUNT: usize = 19;

    /// Try to convert a global name to a Builtin slot.
    #[inline]
    pub const fn from_name(name: &str) -> Option<Self> {
        // Use a const-compatible match on bytes
        let bytes = name.as_bytes();
        match bytes {
            b"print" => Some(Builtin::Print),
            b"type" => Some(Builtin::Type),
            b"tonumber" => Some(Builtin::Tonumber),
            b"tostring" => Some(Builtin::Tostring),
            b"pairs" => Some(Builtin::Pairs),
            b"ipairs" => Some(Builtin::Ipairs),
            b"next" => Some(Builtin::Next),
            b"getmetatable" => Some(Builtin::Getmetatable),
            b"setmetatable" => Some(Builtin::Setmetatable),
            b"rawget" => Some(Builtin::Rawget),
            b"rawset" => Some(Builtin::Rawset),
            b"rawequal" => Some(Builtin::Rawequal),
            b"rawlen" => Some(Builtin::Rawlen),
            b"select" => Some(Builtin::Select),
            b"unpack" => Some(Builtin::Unpack),
            b"math" => Some(Builtin::Math),
            b"string" => Some(Builtin::String),
            b"table" => Some(Builtin::Table),
            b"_G" => Some(Builtin::G),
            _ => None,
        }
    }

    /// Get the name of this builtin.
    #[inline]
    pub const fn name(self) -> &'static str {
        match self {
            Builtin::Print => "print",
            Builtin::Type => "type",
            Builtin::Tonumber => "tonumber",
            Builtin::Tostring => "tostring",
            Builtin::Pairs => "pairs",
            Builtin::Ipairs => "ipairs",
            Builtin::Next => "next",
            Builtin::Getmetatable => "getmetatable",
            Builtin::Setmetatable => "setmetatable",
            Builtin::Rawget => "rawget",
            Builtin::Rawset => "rawset",
            Builtin::Rawequal => "rawequal",
            Builtin::Rawlen => "rawlen",
            Builtin::Select => "select",
            Builtin::Unpack => "unpack",
            Builtin::Math => "math",
            Builtin::String => "string",
            Builtin::Table => "table",
            Builtin::G => "_G",
        }
    }

    /// Convert to u8 for instruction encoding.
    #[inline]
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// Convert from u8. Returns None if out of range.
    #[inline]
    pub const fn from_u8(n: u8) -> Option<Self> {
        if n < Self::COUNT as u8 {
            // SAFETY: n is in range and repr(u8) ensures valid transmute
            Some(unsafe { std::mem::transmute(n) })
        } else {
            None
        }
    }
}

/// A 32-bit encoded instruction.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub(crate) struct Instr(u32);

// Opcode constants
impl Instr {
    // No operands
    pub(crate) const OP_POP: u8 = 0;
    pub(crate) const OP_DUP: u8 = 1;
    pub(crate) const OP_SWAP: u8 = 2;
    pub(crate) const OP_NEW_TABLE: u8 = 3;
    pub(crate) const OP_GET_TABLE: u8 = 4;
    pub(crate) const OP_ADD: u8 = 5;
    pub(crate) const OP_SUBTRACT: u8 = 6;
    pub(crate) const OP_MULTIPLY: u8 = 7;
    pub(crate) const OP_DIVIDE: u8 = 8;
    pub(crate) const OP_POW: u8 = 9;
    pub(crate) const OP_MOD: u8 = 10;
    pub(crate) const OP_CONCAT: u8 = 11;
    pub(crate) const OP_LESS: u8 = 12;
    pub(crate) const OP_LESS_EQUAL: u8 = 13;
    pub(crate) const OP_GREATER: u8 = 14;
    pub(crate) const OP_GREATER_EQUAL: u8 = 15;
    pub(crate) const OP_EQUAL: u8 = 16;
    pub(crate) const OP_NOT_EQUAL: u8 = 17;
    pub(crate) const OP_NOT: u8 = 18;
    pub(crate) const OP_LENGTH: u8 = 19;
    pub(crate) const OP_NEGATE: u8 = 20;
    pub(crate) const OP_MARK_CALL_BASE: u8 = 21;
    pub(crate) const OP_PUSH_NIL: u8 = 22;

    // One u8 operand (in A slot)
    pub(crate) const OP_GET_GLOBAL: u8 = 30;
    pub(crate) const OP_SET_GLOBAL: u8 = 31;
    pub(crate) const OP_GET_LOCAL: u8 = 32;
    pub(crate) const OP_SET_LOCAL: u8 = 33;
    pub(crate) const OP_GET_UPVALUE: u8 = 34;
    pub(crate) const OP_SET_UPVALUE: u8 = 35;
    pub(crate) const OP_GET_FIELD: u8 = 36;
    pub(crate) const OP_INIT_INDEX: u8 = 37;
    pub(crate) const OP_PUSH_NUM: u8 = 38;
    pub(crate) const OP_PUSH_STRING: u8 = 39;
    pub(crate) const OP_TFOR_PREP: u8 = 40;
    pub(crate) const OP_RETURN: u8 = 41;
    pub(crate) const OP_CLOSE_UPVALUES: u8 = 42;
    pub(crate) const OP_CLOSURE: u8 = 43;
    pub(crate) const OP_VARARG: u8 = 44;
    pub(crate) const OP_SET_LIST: u8 = 45;
    pub(crate) const OP_SET_TABLE: u8 = 46;
    pub(crate) const OP_PUSH_BOOL: u8 = 47; // A=0 for false, A=1 for true
    pub(crate) const OP_GET_BUILTIN: u8 = 48; // A=builtin slot index
    pub(crate) const OP_SET_BUILTIN: u8 = 49; // A=builtin slot index

    // Two u8 operands (in A and B slots)
    pub(crate) const OP_SET_FIELD: u8 = 50;
    pub(crate) const OP_INIT_FIELD: u8 = 51;
    pub(crate) const OP_TFOR_CALL: u8 = 52;
    pub(crate) const OP_CALL: u8 = 53;

    // Jump instructions (signed 16-bit offset in B+C slots)
    pub(crate) const OP_JUMP: u8 = 60;
    pub(crate) const OP_BRANCH_FALSE: u8 = 61;
    pub(crate) const OP_BRANCH_TRUE_KEEP: u8 = 62;
    pub(crate) const OP_BRANCH_FALSE_KEEP: u8 = 63;

    // u8 + signed 16-bit offset (A slot + sBx)
    pub(crate) const OP_FOR_PREP: u8 = 70;
    pub(crate) const OP_FOR_LOOP: u8 = 71;
    pub(crate) const OP_TFOR_LOOP: u8 = 72;
}

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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub(crate) const fn c(self) -> u8 {
        self.0 as u8
    }

    /// Get the signed 16-bit offset (sBx).
    #[inline]
    pub(crate) const fn sbx(self) -> i16 {
        self.0 as u16 as i16
    }

    /// Get the raw u32 value.
    #[inline]
    #[allow(dead_code)]
    pub(crate) const fn raw(self) -> u32 {
        self.0
    }
}

// Convenient constructors for each instruction type
impl Instr {
    // No operands
    pub(crate) const fn pop() -> Self { Self::op(Self::OP_POP) }
    pub(crate) const fn dup() -> Self { Self::op(Self::OP_DUP) }
    pub(crate) const fn swap() -> Self { Self::op(Self::OP_SWAP) }
    pub(crate) const fn new_table() -> Self { Self::op(Self::OP_NEW_TABLE) }
    pub(crate) const fn get_table() -> Self { Self::op(Self::OP_GET_TABLE) }
    pub(crate) const fn add() -> Self { Self::op(Self::OP_ADD) }
    pub(crate) const fn subtract() -> Self { Self::op(Self::OP_SUBTRACT) }
    pub(crate) const fn multiply() -> Self { Self::op(Self::OP_MULTIPLY) }
    pub(crate) const fn divide() -> Self { Self::op(Self::OP_DIVIDE) }
    pub(crate) const fn pow() -> Self { Self::op(Self::OP_POW) }
    pub(crate) const fn modulo() -> Self { Self::op(Self::OP_MOD) }
    pub(crate) const fn concat() -> Self { Self::op(Self::OP_CONCAT) }
    pub(crate) const fn less() -> Self { Self::op(Self::OP_LESS) }
    pub(crate) const fn less_equal() -> Self { Self::op(Self::OP_LESS_EQUAL) }
    pub(crate) const fn greater() -> Self { Self::op(Self::OP_GREATER) }
    pub(crate) const fn greater_equal() -> Self { Self::op(Self::OP_GREATER_EQUAL) }
    pub(crate) const fn equal() -> Self { Self::op(Self::OP_EQUAL) }
    pub(crate) const fn not_equal() -> Self { Self::op(Self::OP_NOT_EQUAL) }
    pub(crate) const fn not() -> Self { Self::op(Self::OP_NOT) }
    pub(crate) const fn length() -> Self { Self::op(Self::OP_LENGTH) }
    pub(crate) const fn negate() -> Self { Self::op(Self::OP_NEGATE) }
    pub(crate) const fn mark_call_base(adjustment: u8) -> Self { Self::op_a(Self::OP_MARK_CALL_BASE, adjustment) }
    pub(crate) const fn push_nil() -> Self { Self::op(Self::OP_PUSH_NIL) }

    // One u8 operand
    pub(crate) const fn get_global(idx: u8) -> Self { Self::op_a(Self::OP_GET_GLOBAL, idx) }
    pub(crate) const fn set_global(idx: u8) -> Self { Self::op_a(Self::OP_SET_GLOBAL, idx) }
    pub(crate) const fn get_local(slot: u8) -> Self { Self::op_a(Self::OP_GET_LOCAL, slot) }
    pub(crate) const fn set_local(slot: u8) -> Self { Self::op_a(Self::OP_SET_LOCAL, slot) }
    pub(crate) const fn get_upvalue(idx: u8) -> Self { Self::op_a(Self::OP_GET_UPVALUE, idx) }
    pub(crate) const fn set_upvalue(idx: u8) -> Self { Self::op_a(Self::OP_SET_UPVALUE, idx) }
    pub(crate) const fn get_field(idx: u8) -> Self { Self::op_a(Self::OP_GET_FIELD, idx) }
    pub(crate) const fn init_index(offset: u8) -> Self { Self::op_a(Self::OP_INIT_INDEX, offset) }
    pub(crate) const fn push_num(idx: u8) -> Self { Self::op_a(Self::OP_PUSH_NUM, idx) }
    pub(crate) const fn push_string(idx: u8) -> Self { Self::op_a(Self::OP_PUSH_STRING, idx) }
    pub(crate) const fn tfor_prep(slot: u8) -> Self { Self::op_a(Self::OP_TFOR_PREP, slot) }
    pub(crate) const fn ret(n: RetCount) -> Self { Self::op_a(Self::OP_RETURN, n.to_u8()) }
    pub(crate) const fn close_upvalues(level: u8) -> Self { Self::op_a(Self::OP_CLOSE_UPVALUES, level) }
    pub(crate) const fn closure(idx: u8) -> Self { Self::op_a(Self::OP_CLOSURE, idx) }
    pub(crate) const fn vararg(n: u8) -> Self { Self::op_a(Self::OP_VARARG, n) }
    pub(crate) const fn set_list(count: u8) -> Self { Self::op_a(Self::OP_SET_LIST, count) }
    pub(crate) const fn set_table(offset: u8) -> Self { Self::op_a(Self::OP_SET_TABLE, offset) }
    pub(crate) const fn push_bool(b: bool) -> Self { Self::op_a(Self::OP_PUSH_BOOL, b as u8) }
    pub(crate) const fn get_builtin(slot: Builtin) -> Self { Self::op_a(Self::OP_GET_BUILTIN, slot.to_u8()) }
    pub(crate) const fn set_builtin(slot: Builtin) -> Self { Self::op_a(Self::OP_SET_BUILTIN, slot.to_u8()) }

    // Two u8 operands
    pub(crate) const fn set_field(offset: u8, idx: u8) -> Self { Self::op_ab(Self::OP_SET_FIELD, offset, idx) }
    pub(crate) const fn init_field(offset: u8, idx: u8) -> Self { Self::op_ab(Self::OP_INIT_FIELD, offset, idx) }
    pub(crate) const fn tfor_call(slot: u8, num_vars: u8) -> Self { Self::op_ab(Self::OP_TFOR_CALL, slot, num_vars) }
    pub(crate) const fn call(num_args: ArgCount, num_rets: RetCount) -> Self {
        Self::op_ab(Self::OP_CALL, num_args.to_u8(), num_rets.to_u8())
    }

    // Jump instructions (signed 16-bit offset)
    pub(crate) const fn jump(offset: i16) -> Self { Self::op_sbx(Self::OP_JUMP, offset) }
    pub(crate) const fn branch_false(offset: i16) -> Self { Self::op_sbx(Self::OP_BRANCH_FALSE, offset) }
    pub(crate) const fn branch_true_keep(offset: i16) -> Self { Self::op_sbx(Self::OP_BRANCH_TRUE_KEEP, offset) }
    pub(crate) const fn branch_false_keep(offset: i16) -> Self { Self::op_sbx(Self::OP_BRANCH_FALSE_KEEP, offset) }

    // u8 + signed 16-bit offset
    pub(crate) const fn for_prep(slot: u8, offset: i16) -> Self { Self::op_a_sbx(Self::OP_FOR_PREP, slot, offset) }
    pub(crate) const fn for_loop(slot: u8, offset: i16) -> Self { Self::op_a_sbx(Self::OP_FOR_LOOP, slot, offset) }
    pub(crate) const fn tfor_loop(slot: u8, offset: i16) -> Self { Self::op_a_sbx(Self::OP_TFOR_LOOP, slot, offset) }
}

impl std::fmt::Debug for Instr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.opcode() {
            Self::OP_POP => write!(f, "Pop"),
            Self::OP_DUP => write!(f, "Dup"),
            Self::OP_SWAP => write!(f, "Swap"),
            Self::OP_NEW_TABLE => write!(f, "NewTable"),
            Self::OP_GET_TABLE => write!(f, "GetTable"),
            Self::OP_ADD => write!(f, "Add"),
            Self::OP_SUBTRACT => write!(f, "Subtract"),
            Self::OP_MULTIPLY => write!(f, "Multiply"),
            Self::OP_DIVIDE => write!(f, "Divide"),
            Self::OP_POW => write!(f, "Pow"),
            Self::OP_MOD => write!(f, "Mod"),
            Self::OP_CONCAT => write!(f, "Concat"),
            Self::OP_LESS => write!(f, "Less"),
            Self::OP_LESS_EQUAL => write!(f, "LessEqual"),
            Self::OP_GREATER => write!(f, "Greater"),
            Self::OP_GREATER_EQUAL => write!(f, "GreaterEqual"),
            Self::OP_EQUAL => write!(f, "Equal"),
            Self::OP_NOT_EQUAL => write!(f, "NotEqual"),
            Self::OP_NOT => write!(f, "Not"),
            Self::OP_LENGTH => write!(f, "Length"),
            Self::OP_NEGATE => write!(f, "Negate"),
            Self::OP_MARK_CALL_BASE => write!(f, "MarkCallBase"),
            Self::OP_PUSH_NIL => write!(f, "PushNil"),
            Self::OP_GET_GLOBAL => write!(f, "GetGlobal({})", self.a()),
            Self::OP_SET_GLOBAL => write!(f, "SetGlobal({})", self.a()),
            Self::OP_GET_LOCAL => write!(f, "GetLocal({})", self.a()),
            Self::OP_SET_LOCAL => write!(f, "SetLocal({})", self.a()),
            Self::OP_GET_UPVALUE => write!(f, "GetUpvalue({})", self.a()),
            Self::OP_SET_UPVALUE => write!(f, "SetUpvalue({})", self.a()),
            Self::OP_GET_FIELD => write!(f, "GetField({})", self.a()),
            Self::OP_INIT_INDEX => write!(f, "InitIndex({})", self.a()),
            Self::OP_PUSH_NUM => write!(f, "PushNum({})", self.a()),
            Self::OP_PUSH_STRING => write!(f, "PushString({})", self.a()),
            Self::OP_TFOR_PREP => write!(f, "TForPrep({})", self.a()),
            Self::OP_RETURN => write!(f, "Return({:?})", RetCount::from_u8(self.a())),
            Self::OP_CLOSE_UPVALUES => write!(f, "CloseUpvalues({})", self.a()),
            Self::OP_CLOSURE => write!(f, "Closure({})", self.a()),
            Self::OP_VARARG => write!(f, "Vararg({})", self.a()),
            Self::OP_SET_LIST => write!(f, "SetList({})", self.a()),
            Self::OP_SET_TABLE => write!(f, "SetTable({})", self.a()),
            Self::OP_PUSH_BOOL => write!(f, "PushBool({})", self.a() != 0),
            Self::OP_GET_BUILTIN => write!(f, "GetBuiltin({:?})", Builtin::from_u8(self.a())),
            Self::OP_SET_BUILTIN => write!(f, "SetBuiltin({:?})", Builtin::from_u8(self.a())),
            Self::OP_SET_FIELD => write!(f, "SetField({}, {})", self.a(), self.b()),
            Self::OP_INIT_FIELD => write!(f, "InitField({}, {})", self.a(), self.b()),
            Self::OP_TFOR_CALL => write!(f, "TForCall({}, {})", self.a(), self.b()),
            Self::OP_CALL => write!(f, "Call({:?}, {:?})", ArgCount::from_u8(self.a()), RetCount::from_u8(self.b())),
            Self::OP_JUMP => write!(f, "Jump({})", self.sbx()),
            Self::OP_BRANCH_FALSE => write!(f, "BranchFalse({})", self.sbx()),
            Self::OP_BRANCH_TRUE_KEEP => write!(f, "BranchTrueKeep({})", self.sbx()),
            Self::OP_BRANCH_FALSE_KEEP => write!(f, "BranchFalseKeep({})", self.sbx()),
            Self::OP_FOR_PREP => write!(f, "ForPrep({}, {})", self.a(), self.sbx()),
            Self::OP_FOR_LOOP => write!(f, "ForLoop({}, {})", self.a(), self.sbx()),
            Self::OP_TFOR_LOOP => write!(f, "TForLoop({}, {})", self.a(), self.sbx()),
            op => write!(f, "Unknown(op={}, raw={:#x})", op, self.0),
        }
    }
}
