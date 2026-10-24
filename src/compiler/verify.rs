use super::{Bytecode, UpvalueDesc};
use crate::instr::{ArithOp, ArithOperands, Builtin, Instr, OPEN_COUNT};
use crate::limits::{
    MAX_FIXED_ARGS, MAX_FIXED_RESULTS, MAX_FRAME_SLOTS, MAX_LITERALS, MAX_NESTED,
    MAX_TABLE_TEMPLATE_FIELDS, MAX_TABLE_TEMPLATES, MAX_UPVALUES,
};

/// Why the bytecode verifier rejected a function. Typed so neither the
/// compiler's internal error nor a snapshot rejection carries a free-form
/// string; public as the reason inside a snapshot's
/// `InvalidBytecodeReason::Validation`.
// Public through `dellingr::snapshot`, which exists only with the snapshot
// feature; the compiler uses the type in every build.
#[cfg_attr(not(feature = "snapshot"), allow(unreachable_pub))]
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationReason {
    /// child upvalue descriptor is invalid for parent
    ChildUpvalueInvalid,
    /// nested chunk depth exceeds parser limit
    ChunkDepthExceeded,
    /// a register operand or range is beyond the frame
    RegisterOutOfRange,
    /// the frame reserves fewer registers than its parameters and locals
    FrameTooSmall,
    /// the frame size uses the reserved open-count encoding
    FrameSizeUnencodable,
    /// a count operand is neither a valid fixed count nor open
    CountOperandInvalid,
    /// an instruction leaving open results is not followed by its consumer
    OpenResultsNotConsumed,
    /// an instruction reading open results does not follow their producer
    OpenResultsWithoutProducer,
    /// jump target is out of range
    JumpTargetOutOfRange,
    /// code is empty
    EmptyCode,
    /// line info length does not match code length
    LineInfoLengthMismatch,
    /// code does not end in return
    MissingFinalReturn,
    /// unknown opcode
    UnknownOpcode,
    /// global name operand is not UTF-8
    GlobalNameNotUtf8,
    /// global cache slot does not match first-use order
    GlobalCacheSlotOrder,
    /// builtin slot is out of range
    BuiltinSlotOutOfRange,
    /// TFOR_CALL requires at least one result
    TforCallWithoutResults,
    /// TFOR_CALL result count exceeds the fixed-count limit
    TforCallResultCountExceeded,
    /// vararg instruction in non-vararg function
    VarargInNonVarargFunction,
    /// boolean operand is invalid
    BooleanOperandInvalid,
    /// concat requires at least two values
    ConcatTooFewValues,
    /// declared cache slots do not match instructions
    CacheSlotCountMismatch,
    /// An operand indexes past its pool or table.
    IndexOutOfRange {
        /// The pool the operand indexes.
        operand: BytecodeOperand,
    },
    /// A pool or descriptor list exceeds its limit.
    TooMany {
        /// The list that is too long.
        what: BytecodePool,
        /// Its maximum length.
        limit: usize,
    },
    /// A table template has more fields than the template limit.
    TableTemplateTooLarge,
    /// Parameters plus locals exceed the frame slot limit.
    FrameSlotCountExceeded,
    /// A table template key does not name a string literal.
    TemplateKeyInvalid {
        /// Index of the template.
        template: usize,
    },
    /// A table template names one key twice.
    TemplateKeyDuplicate {
        /// Index of the template.
        template: usize,
    },
    /// A cache family's slots are not allocated in instruction order.
    CacheSlotOrder {
        /// The cache family.
        family: CacheFamily,
    },
    /// An instruction's reserved operand bits are not zero.
    ReservedBitsNonZero {
        /// The instruction's opcode byte.
        opcode: u8,
        /// The whole instruction word.
        raw: u64,
    },
}

/// The pool an instruction operand indexes, named by
/// [`ValidationReason::IndexOutOfRange`].
// Public through `dellingr::snapshot`, which exists only with the snapshot
// feature; the compiler uses the type in every build.
#[cfg_attr(not(feature = "snapshot"), allow(unreachable_pub))]
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BytecodeOperand {
    /// The number literal pool.
    NumberLiteral,
    /// The string literal pool.
    StringLiteral,
    /// The table template list.
    TableTemplate,
    /// The nested function list.
    NestedChunk,
    /// The upvalue list.
    Upvalue,
}

impl std::fmt::Display for BytecodeOperand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NumberLiteral => "number literal",
            Self::StringLiteral => "string literal",
            Self::TableTemplate => "table template",
            Self::NestedChunk => "nested chunk",
            Self::Upvalue => "upvalue",
        })
    }
}

/// A per-function list with a length limit, named by
/// [`ValidationReason::TooMany`].
// Public through `dellingr::snapshot`, which exists only with the snapshot
// feature; the compiler uses the type in every build.
#[cfg_attr(not(feature = "snapshot"), allow(unreachable_pub))]
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BytecodePool {
    /// Table templates.
    TableTemplate,
    /// Nested functions.
    NestedChunk,
    /// Upvalue descriptors.
    UpvalueDescriptor,
    /// Number or string literals; each pool has the same limit.
    Literal,
}

impl std::fmt::Display for BytecodePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TableTemplate => "table template",
            Self::NestedChunk => "nested chunk",
            Self::UpvalueDescriptor => "upvalue descriptor",
            Self::Literal => "literal",
        })
    }
}

/// An inline-cache family whose slots are allocated in instruction order,
/// named by [`ValidationReason::CacheSlotOrder`]. Global caches have their
/// own rule ([`ValidationReason::GlobalCacheSlotOrder`]).
// Public through `dellingr::snapshot`, which exists only with the snapshot
// feature; the compiler uses the type in every build.
#[cfg_attr(not(feature = "snapshot"), allow(unreachable_pub))]
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheFamily {
    /// `GET_FIELD` and `SELF` caches.
    Field,
    /// `SET_FIELD` caches.
    SetField,
    /// Generic-`for` `pairs` cursors.
    TforCursor,
}

impl std::fmt::Display for CacheFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Field => "field",
            Self::SetField => "set-field",
            Self::TforCursor => "TFOR_CALL cursor",
        })
    }
}

impl std::fmt::Display for ValidationReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::ChildUpvalueInvalid => "child upvalue descriptor is invalid for parent",
            Self::ChunkDepthExceeded => "nested chunk depth exceeds parser limit",
            Self::RegisterOutOfRange => "register operand is beyond the frame",
            Self::FrameTooSmall => "frame reserves fewer registers than its locals",
            Self::FrameSizeUnencodable => "frame size uses the reserved open-count encoding",
            Self::CountOperandInvalid => "count operand is invalid",
            Self::OpenResultsNotConsumed => "open results are not consumed by the next instruction",
            Self::OpenResultsWithoutProducer => "open results are read without a producer",
            Self::JumpTargetOutOfRange => "jump target is out of range",
            Self::EmptyCode => "code is empty",
            Self::LineInfoLengthMismatch => "line info length does not match code length",
            Self::MissingFinalReturn => "code does not end in return",
            Self::UnknownOpcode => "unknown opcode",
            Self::GlobalNameNotUtf8 => "global name operand is not UTF-8",
            Self::GlobalCacheSlotOrder => "global cache slot does not match first-use order",
            Self::BuiltinSlotOutOfRange => "builtin slot is out of range",
            Self::TforCallWithoutResults => "TFOR_CALL requires at least one result",
            Self::TforCallResultCountExceeded => "TFOR_CALL result count exceeds 255",
            Self::VarargInNonVarargFunction => "vararg instruction in non-vararg function",
            Self::BooleanOperandInvalid => "boolean operand is invalid",
            Self::ConcatTooFewValues => "concat requires at least two values",
            Self::CacheSlotCountMismatch => "declared cache slots do not match instructions",
            Self::IndexOutOfRange { operand } => {
                return write!(f, "{operand} index is out of range");
            }
            Self::TooMany { what, limit } => {
                return write!(f, "too many {what}s (limit {limit})");
            }
            Self::TableTemplateTooLarge => {
                return write!(
                    f,
                    "table template exceeds limit {MAX_TABLE_TEMPLATE_FIELDS}"
                );
            }
            Self::FrameSlotCountExceeded => {
                return write!(f, "frame slot count exceeds {MAX_FRAME_SLOTS}");
            }
            Self::TemplateKeyInvalid { template } => {
                return write!(f, "table template {template} has an invalid string key");
            }
            Self::TemplateKeyDuplicate { template } => {
                return write!(f, "table template {template} has a duplicate key");
            }
            Self::CacheSlotOrder { family } => {
                return write!(f, "{family} cache slot does not match instruction order");
            }
            Self::ReservedBitsNonZero { opcode, raw } => {
                return write!(
                    f,
                    "reserved operand bits are non-zero (opcode {opcode}, raw {raw:016x})"
                );
            }
        };
        f.write_str(text)
    }
}

#[derive(Debug)]
pub(crate) struct BytecodeValidationError {
    pub(crate) instruction: Option<u32>,
    pub(crate) reason: ValidationReason,
}

impl std::fmt::Display for BytecodeValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.instruction {
            Some(instruction) => write!(f, "at instruction {instruction}: {}", self.reason),
            None => write!(f, "at chunk metadata: {}", self.reason),
        }
    }
}

/// Check the enclosing-frame contract shared by compiled and saved chunks.
/// Arena references and cycle detection belong to the snapshot representation.
pub(crate) fn validate_child(
    parent: &impl BytecodeView,
    child: &impl BytecodeView,
) -> Result<(), BytecodeValidationError> {
    for index in 0..child.upvalues_len() {
        let valid = match child.upvalue(index) {
            UpvalueDesc::Local(slot) => {
                usize::from(slot)
                    < usize::from(parent.num_params()) + usize::from(parent.num_locals())
            }
            UpvalueDesc::Upvalue(slot) => usize::from(slot) < parent.upvalues_len(),
        };
        if !valid {
            return Err(err(None, ValidationReason::ChildUpvalueInvalid));
        }
    }
    Ok(())
}

pub(crate) fn validate_depth(depth: usize) -> Result<(), BytecodeValidationError> {
    if depth > crate::limits::MAX_CHUNK_NESTING {
        return Err(err(None, ValidationReason::ChunkDepthExceeded));
    }
    Ok(())
}

pub(crate) trait BytecodeView {
    fn code_len(&self) -> usize;
    fn raw_instruction(&self, pc: usize) -> u64;
    fn number_literals_len(&self) -> usize;
    fn string_literals(&self) -> &[Vec<u8>];
    fn table_templates(&self) -> &[Vec<u16>];
    fn global_cache_slots(&self) -> u8;
    fn field_cache_slots(&self) -> u8;
    fn set_field_cache_slots(&self) -> u8;
    fn tfor_cursor_slots(&self) -> u8;
    fn num_params(&self) -> u8;
    fn num_locals(&self) -> u8;
    fn max_stack(&self) -> u16;
    fn nested_len(&self) -> usize;
    fn upvalues_len(&self) -> usize;
    fn upvalue(&self, index: usize) -> UpvalueDesc;
    fn is_vararg(&self) -> bool;
    fn line_info_len(&self) -> usize;
}

impl BytecodeView for Bytecode {
    fn code_len(&self) -> usize {
        self.code.len()
    }
    fn raw_instruction(&self, pc: usize) -> u64 {
        self.code[pc].raw()
    }
    fn number_literals_len(&self) -> usize {
        self.number_literals.len()
    }
    fn string_literals(&self) -> &[Vec<u8>] {
        &self.string_literals
    }
    fn table_templates(&self) -> &[Vec<u16>] {
        &self.table_templates
    }
    fn global_cache_slots(&self) -> u8 {
        self.global_cache_slots
    }
    fn field_cache_slots(&self) -> u8 {
        self.field_cache_slots
    }
    fn set_field_cache_slots(&self) -> u8 {
        self.set_field_cache_slots
    }
    fn tfor_cursor_slots(&self) -> u8 {
        self.tfor_cursor_slots
    }
    fn num_params(&self) -> u8 {
        self.num_params
    }
    fn num_locals(&self) -> u8 {
        self.num_locals
    }
    fn max_stack(&self) -> u16 {
        self.max_stack
    }
    fn nested_len(&self) -> usize {
        self.nested.len()
    }
    fn upvalues_len(&self) -> usize {
        self.upvalues.len()
    }
    fn upvalue(&self, index: usize) -> UpvalueDesc {
        self.upvalues[index]
    }
    fn is_vararg(&self) -> bool {
        self.is_vararg
    }
    fn line_info_len(&self) -> usize {
        self.line_info.len()
    }
}

fn err(pc: Option<usize>, reason: ValidationReason) -> BytecodeValidationError {
    BytecodeValidationError {
        instruction: pc.map(|pc| pc as u32),
        reason,
    }
}

fn check_index(
    pc: usize,
    value: u16,
    len: usize,
    operand: BytecodeOperand,
) -> Result<(), BytecodeValidationError> {
    if (value as usize) < len {
        Ok(())
    } else {
        Err(err(Some(pc), ValidationReason::IndexOutOfRange { operand }))
    }
}

/// The registers `first..first + count` exist in a frame of `frame` registers.
fn check_registers(
    pc: usize,
    first: u16,
    count: usize,
    frame: usize,
) -> Result<(), BytecodeValidationError> {
    if usize::from(first)
        .checked_add(count)
        .is_some_and(|end| end <= frame)
    {
        Ok(())
    } else {
        Err(err(Some(pc), ValidationReason::RegisterOutOfRange))
    }
}

/// A fixed count operand, or `None` when it is the open sentinel. Counts
/// above `limit` other than the sentinel are corrupt.
fn count_operand(
    pc: usize,
    value: u16,
    limit: usize,
) -> Result<Option<usize>, BytecodeValidationError> {
    if value == OPEN_COUNT {
        Ok(None)
    } else if usize::from(value) <= limit {
        Ok(Some(usize::from(value)))
    } else {
        Err(err(Some(pc), ValidationReason::CountOperandInvalid))
    }
}

/// The absolute target of a transfer at `pc`, if it lies in the code.
fn jump_target(pc: usize, inst: Instr, code_len: usize) -> Option<usize> {
    pc.checked_add(1)?
        .checked_add_signed(inst.sj() as isize)
        .filter(|&target| target < code_len)
}

// Dispatch policy: verify static operand bounds, register ranges, the
// open-results protocol and control flow here at admission, then trust them
// during execution. This does not prove runtime value types: constructor
// operands, numeric loop values, callable handlers and host/resource limits
// still need fallible runtime checks. A regression in these static proofs is
// a VM bug, not a recoverable Lua error.
//
// Every register an instruction names lies inside the frame the function
// reserves (`max_stack`), so execution indexes registers without checking.
// Open results are the only place the stack top differs from the frame top
// between instructions: a producer (a call asking for all results, a vararg
// of every value) must be followed by its consumer (a call with open
// arguments, an open return or set-list) on the very next instruction, which
// no jump may enter, and the consumer's own operand range must start at or
// below the producer's.
pub(crate) fn validate_bytecode(view: &impl BytecodeView) -> Result<(), BytecodeValidationError> {
    let code_len = view.code_len();
    if code_len == 0 {
        return Err(err(None, ValidationReason::EmptyCode));
    }
    if view.line_info_len() != code_len {
        return Err(err(None, ValidationReason::LineInfoLengthMismatch));
    }
    for (name, len, limit) in [
        (
            BytecodePool::TableTemplate,
            view.table_templates().len(),
            MAX_TABLE_TEMPLATES,
        ),
        (BytecodePool::NestedChunk, view.nested_len(), MAX_NESTED),
        (
            BytecodePool::UpvalueDescriptor,
            view.upvalues_len(),
            MAX_UPVALUES,
        ),
    ] {
        if len > limit {
            return Err(err(None, ValidationReason::TooMany { what: name, limit }));
        }
    }
    if view.number_literals_len() > MAX_LITERALS || view.string_literals().len() > MAX_LITERALS {
        return Err(err(
            None,
            ValidationReason::TooMany {
                what: BytecodePool::Literal,
                limit: MAX_LITERALS,
            },
        ));
    }
    if view
        .table_templates()
        .iter()
        .any(|template| template.len() > MAX_TABLE_TEMPLATE_FIELDS)
    {
        return Err(err(None, ValidationReason::TableTemplateTooLarge));
    }
    let slots = view.num_params() as usize + view.num_locals() as usize;
    if slots > MAX_FRAME_SLOTS {
        return Err(err(None, ValidationReason::FrameSlotCountExceeded));
    }
    let frame = usize::from(view.max_stack());
    if view.max_stack() == OPEN_COUNT {
        return Err(err(None, ValidationReason::FrameSizeUnencodable));
    }
    if frame < slots {
        return Err(err(None, ValidationReason::FrameTooSmall));
    }
    if !matches!(
        Instr::from_raw(view.raw_instruction(code_len - 1)).opcode(),
        Instr::OP_RETURN
    ) {
        return Err(err(
            Some(code_len - 1),
            ValidationReason::MissingFinalReturn,
        ));
    }
    for (template_idx, template) in view.table_templates().iter().enumerate() {
        // Keys are compared by content, not id: two equal-content string
        // literals intern to one runtime key, and `Table::with_template_keys`
        // relies on every template key being distinct (the parser guarantees
        // it; a forged snapshot must not get past this check without it).
        let mut seen: std::collections::BTreeSet<&[u8]> = std::collections::BTreeSet::new();
        for key in template {
            let Some(bytes) = view.string_literals().get(*key as usize) else {
                return Err(err(
                    None,
                    ValidationReason::TemplateKeyInvalid {
                        template: template_idx,
                    },
                ));
            };
            if !seen.insert(bytes.as_slice()) {
                return Err(err(
                    None,
                    ValidationReason::TemplateKeyDuplicate {
                        template: template_idx,
                    },
                ));
            }
        }
    }

    // Jump targets, needed to keep jumps out of open-results windows.
    let mut targeted = vec![false; code_len];
    for pc in 0..code_len {
        let inst = Instr::from_raw(view.raw_instruction(pc));
        if Instr::is_transfer(inst.opcode()) {
            match jump_target(pc, inst, code_len) {
                Some(target) => targeted[target] = true,
                None => return Err(err(Some(pc), ValidationReason::JumpTargetOutOfRange)),
            }
        }
    }

    let strings = view.string_literals().len();
    let numbers = view.number_literals_len();
    let mut global_slots: Vec<Option<usize>> = vec![None; strings];
    let mut global_name_checked = vec![false; strings];
    let mut next_global_slot = 0usize;
    let mut next_field_slot = 0usize;
    let mut next_set_field_slot = 0usize;
    let mut next_tfor_cursor_slot = 0usize;
    // This loop deliberately restates the cache-slot allocation policy of
    // `Bytecode::assign_cache_slots` (first-use order for globals, a fresh
    // global slot per SET_GLOBAL, instruction order for fields and methods,
    // set-fields and TFOR_CALL cursors). The duplication is the point: the
    // verifier is the independent check on a snapshot's cached operands, and
    // a shared helper would let a policy bug validate itself. Any change to
    // the allocation policy must change both. Compiler finalization runs this
    // validation on every compile, so a drift between the two fails every
    // compile that reaches the changed case. The operand encoding itself is
    // not restated: both sides decode through the `Instr` cache-slot decoder.
    //
    // Uncached operands are accepted anywhere and consume no slot. Sequential
    // allocation plus the final count equality prove every admitted slot is
    // below its family's declared count, which the runtime cache vectors are
    // sized from. A cache slot owns one site: cache hits trust the slot to
    // describe this site's key, so two sites must never share one.
    let sequential = |pc: usize,
                      slot: Option<u8>,
                      next: &mut usize,
                      family: CacheFamily|
     -> Result<(), BytecodeValidationError> {
        let Some(slot) = slot else {
            return Ok(());
        };
        if usize::from(slot) != *next {
            return Err(err(Some(pc), ValidationReason::CacheSlotOrder { family }));
        }
        *next += 1;
        Ok(())
    };
    let string =
        |pc: usize, value: u16| check_index(pc, value, strings, BytecodeOperand::StringLiteral);
    let number =
        |pc: usize, value: u16| check_index(pc, value, numbers, BytecodeOperand::NumberLiteral);
    let reg = |pc: usize, value: u16| check_registers(pc, value, 1, frame);

    for pc in 0..code_len {
        let inst = Instr::from_raw(view.raw_instruction(pc));
        let op = inst.opcode();
        if !Instr::known_opcode(op) {
            return Err(err(Some(pc), ValidationReason::UnknownOpcode));
        }
        if !inst.reserved_bits_zero() {
            return Err(err(
                Some(pc),
                ValidationReason::ReservedBitsNonZero {
                    opcode: op,
                    raw: inst.raw(),
                },
            ));
        }
        // D carries a cache slot, a method mark or a template entry; the
        // layouts that leave it free reserve it, checked zero above.
        let (a, b, c, d) = (inst.a(), inst.b(), inst.c(), inst.d());

        if let Some((_, operands)) = ArithOp::decode(op) {
            reg(pc, a)?;
            match operands {
                ArithOperands::RegReg => {
                    reg(pc, b)?;
                    reg(pc, c)?;
                }
                ArithOperands::RegConst => {
                    reg(pc, b)?;
                    number(pc, c)?;
                }
                ArithOperands::ConstReg => {
                    number(pc, b)?;
                    reg(pc, c)?;
                }
            }
            continue;
        }

        match op {
            // Two registers: moves, unary operators and compare-and-branch.
            Instr::OP_MOVE
            | Instr::OP_UNM
            | Instr::OP_NOT
            | Instr::OP_LEN
            | Instr::OP_BR_LT
            | Instr::OP_BR_LE
            | Instr::OP_BR_GT
            | Instr::OP_BR_GE
            | Instr::OP_BR_EQ
            | Instr::OP_BR_NE => {
                reg(pc, a)?;
                reg(pc, b)?;
            }
            // Three registers: table reads and writes by key, comparisons.
            Instr::OP_GET_TABLE
            | Instr::OP_SET_TABLE
            | Instr::OP_INIT_INDEX
            | Instr::OP_LT
            | Instr::OP_LE
            | Instr::OP_GT
            | Instr::OP_GE
            | Instr::OP_EQ
            | Instr::OP_NE => {
                reg(pc, a)?;
                reg(pc, b)?;
                reg(pc, c)?;
            }
            // One register.
            Instr::OP_NEW_TABLE | Instr::OP_TEST_FALSE | Instr::OP_TEST_TRUE => reg(pc, a)?,
            Instr::OP_LOAD_NIL => {
                if b == 0 {
                    return Err(err(Some(pc), ValidationReason::CountOperandInvalid));
                }
                check_registers(pc, a, usize::from(b), frame)?;
            }
            Instr::OP_LOAD_BOOL => {
                reg(pc, a)?;
                if b > 1 {
                    return Err(err(Some(pc), ValidationReason::BooleanOperandInvalid));
                }
            }
            Instr::OP_LOAD_NUM => {
                reg(pc, a)?;
                number(pc, b)?;
            }
            Instr::OP_LOAD_STR => {
                reg(pc, a)?;
                string(pc, b)?;
            }
            Instr::OP_GET_UPVAL | Instr::OP_SET_UPVAL => {
                reg(pc, a)?;
                check_index(pc, b, view.upvalues_len(), BytecodeOperand::Upvalue)?;
            }
            Instr::OP_CLOSE => {
                if usize::from(a) > frame {
                    return Err(err(Some(pc), ValidationReason::RegisterOutOfRange));
                }
            }
            Instr::OP_GET_GLOBAL | Instr::OP_SET_GLOBAL => {
                reg(pc, a)?;
                string(pc, b)?;
                // Check each name once: re-scanning a long name at every
                // reference would cost references x name length. The first
                // reference to a bad name still reports.
                if !global_name_checked[b as usize] {
                    if std::str::from_utf8(&view.string_literals()[b as usize]).is_err() {
                        return Err(err(Some(pc), ValidationReason::GlobalNameNotUtf8));
                    }
                    global_name_checked[b as usize] = true;
                }
                // "Uncached" is valid anywhere, not only once the slots are
                // exhausted. The compiler never emits it early, but a
                // hand-built or deliberately uncached snapshot is still
                // semantically valid, and the runtime takes the slow path.
                // An uncached read neither reserves a slot for its name nor
                // marks the name exhausted.
                let global_order_error = || err(Some(pc), ValidationReason::GlobalCacheSlotOrder);
                match (op, inst.cache_slot()) {
                    (_, None) => {}
                    (Instr::OP_GET_GLOBAL, Some(slot)) => {
                        // A cached read reuses its name's slot, or allocates
                        // the next one on the name's first cached read.
                        let expected = *global_slots[b as usize].get_or_insert_with(|| {
                            next_global_slot += 1;
                            next_global_slot - 1
                        });
                        if usize::from(slot) != expected {
                            return Err(global_order_error());
                        }
                    }
                    // A cached write always takes a fresh slot.
                    (_, Some(slot)) => {
                        if usize::from(slot) != next_global_slot {
                            return Err(global_order_error());
                        }
                        next_global_slot += 1;
                    }
                }
            }
            Instr::OP_GET_BUILTIN | Instr::OP_SET_BUILTIN => {
                reg(pc, a)?;
                if usize::from(b) >= Builtin::COUNT {
                    return Err(err(Some(pc), ValidationReason::BuiltinSlotOutOfRange));
                }
            }
            Instr::OP_NEW_TABLE_PRESIZED => {
                reg(pc, a)?;
                if b > u16::from(u8::MAX) {
                    return Err(err(Some(pc), ValidationReason::CountOperandInvalid));
                }
            }
            Instr::OP_NEW_TABLE_TEMPLATE => {
                reg(pc, a)?;
                check_index(
                    pc,
                    b,
                    view.table_templates().len(),
                    BytecodeOperand::TableTemplate,
                )?;
            }
            Instr::OP_GET_FIELD => {
                reg(pc, a)?;
                reg(pc, b)?;
                string(pc, c)?;
                sequential(
                    pc,
                    inst.cache_slot(),
                    &mut next_field_slot,
                    CacheFamily::Field,
                )?;
            }
            Instr::OP_SELF => {
                check_registers(pc, a, 2, frame)?;
                reg(pc, b)?;
                string(pc, c)?;
                sequential(
                    pc,
                    inst.cache_slot(),
                    &mut next_field_slot,
                    CacheFamily::Field,
                )?;
            }
            Instr::OP_SET_FIELD => {
                reg(pc, a)?;
                string(pc, b)?;
                reg(pc, c)?;
                sequential(
                    pc,
                    inst.cache_slot(),
                    &mut next_set_field_slot,
                    CacheFamily::SetField,
                )?;
            }
            Instr::OP_INIT_FIELD | Instr::OP_INIT_FIELD_PINNED => {
                reg(pc, a)?;
                string(pc, b)?;
                reg(pc, c)?;
            }
            Instr::OP_SET_LIST => {
                reg(pc, a)?;
                if let Some(count) = count_operand(pc, b, usize::from(u8::MAX))? {
                    check_registers(pc, a, 1 + count, frame)?;
                }
            }
            Instr::OP_CONCAT => {
                reg(pc, a)?;
                if c < 2 {
                    return Err(err(Some(pc), ValidationReason::ConcatTooFewValues));
                }
                check_registers(pc, b, usize::from(c), frame)?;
            }
            Instr::OP_JMP => {}
            Instr::OP_FOR_PREP | Instr::OP_FOR_LOOP | Instr::OP_TFOR_LOOP => {
                check_registers(pc, a, 4, frame)?;
            }
            Instr::OP_TFOR_CALL => {
                if b > u16::from(u8::MAX) {
                    return Err(err(Some(pc), ValidationReason::TforCallResultCountExceeded));
                }
                if b == 0 {
                    return Err(err(Some(pc), ValidationReason::TforCallWithoutResults));
                }
                check_registers(pc, a, 3 + usize::from(b), frame)?;
                sequential(
                    pc,
                    inst.cache_slot(),
                    &mut next_tfor_cursor_slot,
                    CacheFamily::TforCursor,
                )?;
            }
            Instr::OP_CALL => {
                if d > 1 {
                    return Err(err(Some(pc), ValidationReason::BooleanOperandInvalid));
                }
                // The callee, its fixed arguments and its fixed results.
                let args = count_operand(pc, b, MAX_FIXED_ARGS)?;
                let results = count_operand(pc, c, MAX_FIXED_RESULTS)?;
                check_registers(pc, a, 1 + args.unwrap_or(0), frame)?;
                check_registers(pc, a, results.unwrap_or(0), frame)?;
            }
            Instr::OP_RETURN => match count_operand(pc, b, usize::from(u8::MAX))? {
                Some(count) => check_registers(pc, a, count, frame)?,
                None => reg(pc, a)?,
            },
            Instr::OP_VARARG => {
                if !view.is_vararg() {
                    return Err(err(Some(pc), ValidationReason::VarargInNonVarargFunction));
                }
                let count = count_operand(pc, b, MAX_FIXED_RESULTS)?;
                check_registers(pc, a, count.unwrap_or(1).max(1), frame)?;
            }
            Instr::OP_CLOSURE => {
                reg(pc, a)?;
                check_index(pc, b, view.nested_len(), BytecodeOperand::NestedChunk)?;
            }
            _ => return Err(err(Some(pc), ValidationReason::UnknownOpcode)),
        }

        // The open-results protocol.
        if inst.produces_open() {
            let consumer = pc + 1;
            let ok = consumer < code_len && !targeted[consumer] && {
                let next = Instr::from_raw(view.raw_instruction(consumer));
                next.consumes_open() && open_window_fits(next, inst)
            };
            if !ok {
                return Err(err(Some(pc), ValidationReason::OpenResultsNotConsumed));
            }
        }
        if inst.consumes_open() {
            let ok = pc > 0 && !targeted[pc] && {
                let previous = Instr::from_raw(view.raw_instruction(pc - 1));
                previous.produces_open() && open_window_fits(inst, previous)
            };
            if !ok {
                return Err(err(Some(pc), ValidationReason::OpenResultsWithoutProducer));
            }
        }
    }
    if usize::from(view.global_cache_slots()) != next_global_slot
        || usize::from(view.field_cache_slots()) != next_field_slot
        || usize::from(view.set_field_cache_slots()) != next_set_field_slot
        || usize::from(view.tfor_cursor_slots()) != next_tfor_cursor_slot
    {
        return Err(err(None, ValidationReason::CacheSlotCountMismatch));
    }
    Ok(())
}

/// Whether `consumer` reads a range that starts at or below where `producer`
/// left its values: a call's arguments and a set-list's values start just
/// above their callee or table, a return's values at its own first register.
fn open_window_fits(consumer: Instr, producer: Instr) -> bool {
    let first = match consumer.opcode() {
        Instr::OP_CALL | Instr::OP_SET_LIST => u32::from(consumer.a()) + 1,
        _ => u32::from(consumer.a()),
    };
    first <= u32::from(producer.a())
}

#[cfg(test)]
mod tree_tests {
    use super::{
        Bytecode, CacheFamily, UpvalueDesc, ValidationReason, validate_bytecode, validate_child,
        validate_depth,
    };
    use crate::instr::{ArgCount, Instr, RetCount};

    #[test]
    fn child_captures_are_bounded_by_the_enclosing_frame() {
        let parent = Bytecode {
            num_params: 1,
            num_locals: 1,
            upvalues: vec![UpvalueDesc::Local(0)],
            ..Bytecode::default()
        };
        for (descriptor, valid) in [
            (UpvalueDesc::Local(0), true),
            (UpvalueDesc::Local(1), true),
            (UpvalueDesc::Local(2), false),
            (UpvalueDesc::Upvalue(0), true),
            (UpvalueDesc::Upvalue(1), false),
        ] {
            let child = Bytecode {
                upvalues: vec![descriptor],
                ..Bytecode::default()
            };
            assert_eq!(validate_child(&parent, &child).is_ok(), valid);
        }
    }

    fn chunk(code: Vec<Instr>, max_stack: u16) -> Bytecode {
        Bytecode {
            line_info: vec![1; code.len()],
            code,
            max_stack,
            is_vararg: true,
            ..Bytecode::default()
        }
    }

    fn assert_rejects(bc: &Bytecode, reason: ValidationReason) {
        let error = validate_bytecode(bc).expect_err("malformed bytecode must be rejected");
        assert_eq!(
            error.reason, reason,
            "unexpected validation reason: {error}"
        );
    }

    #[test]
    fn malformed_sizes_report_the_actual_defect() {
        use crate::instr::OPEN_COUNT;

        let reserved = chunk(vec![Instr::ret(0, RetCount::Fixed(0))], OPEN_COUNT);
        assert_eq!(
            validate_bytecode(&reserved)
                .expect_err("reserved frame size")
                .reason,
            ValidationReason::FrameSizeUnencodable
        );
        let mut small = chunk(vec![Instr::ret(0, RetCount::Fixed(0))], 0);
        small.num_params = 1;
        assert_eq!(
            validate_bytecode(&small)
                .expect_err("missing parameter slot")
                .reason,
            ValidationReason::FrameTooSmall
        );
        for (count, reason) in [
            (0, ValidationReason::TforCallWithoutResults),
            (256, ValidationReason::TforCallResultCountExceeded),
            (OPEN_COUNT, ValidationReason::TforCallResultCountExceeded),
        ] {
            let instruction = Instr::from_raw(
                (Instr::tfor_call(0, 1, None).raw() & !(0xffff_u64 << 24))
                    | (u64::from(count) << 24),
            );
            let invalid = chunk(vec![instruction, Instr::ret(0, RetCount::Fixed(0))], 300);
            assert_eq!(
                validate_bytecode(&invalid)
                    .expect_err("invalid result count")
                    .reason,
                reason
            );
        }
    }

    #[test]
    fn registers_must_lie_inside_the_frame() {
        let ok = chunk(vec![Instr::mov(1, 0), Instr::ret(0, RetCount::Fixed(2))], 2);
        assert!(validate_bytecode(&ok).is_ok());
        let beyond = chunk(vec![Instr::mov(2, 0), Instr::ret(0, RetCount::Fixed(0))], 2);
        assert_rejects(&beyond, ValidationReason::RegisterOutOfRange);
        let wide_return = chunk(vec![Instr::ret(1, RetCount::Fixed(2))], 2);
        assert_rejects(&wide_return, ValidationReason::RegisterOutOfRange);
    }

    #[test]
    fn open_results_need_an_adjacent_consumer_no_jump_enters() {
        let call_all = Instr::call(1, ArgCount::Fixed(0), RetCount::All, false);
        let ret_all = Instr::ret(1, RetCount::All);
        let ok = chunk(vec![call_all, ret_all], 4);
        assert!(validate_bytecode(&ok).is_ok());

        // A producer followed by anything else leaves the top open.
        let unconsumed = chunk(vec![call_all, Instr::mov(0, 0), ret_all], 4);
        assert_rejects(&unconsumed, ValidationReason::OpenResultsNotConsumed);

        // A consumer without its producer reads an undefined top.
        let orphan = chunk(vec![Instr::mov(0, 0), ret_all], 4);
        assert_rejects(&orphan, ValidationReason::OpenResultsWithoutProducer);

        // A jump into the consumer bypasses the producer.
        let entered = chunk(vec![Instr::jump(1), call_all, ret_all], 4);
        assert_rejects(&entered, ValidationReason::OpenResultsNotConsumed);

        // The consumer's range must start at or below the producer's values.
        let above = chunk(vec![call_all, Instr::ret(2, RetCount::All)], 4);
        assert_rejects(&above, ValidationReason::OpenResultsNotConsumed);
    }

    /// Each cache family has `MAX_CACHE_SLOTS` cached sites, then an uncached
    /// one. The runtime sizes each cache vector from the declared
    /// count and looks slots up by decoded index, so admission must keep every
    /// cached slot below that count and the count within `MAX_CACHE_SLOTS`.
    #[test]
    fn cache_families_cap_below_the_encodable_limit() {
        use crate::instr::MAX_CACHE_SLOTS;

        type Count = fn(&mut Bytecode) -> &mut u8;
        let sites = |site: &dyn Fn(usize) -> String| {
            (0..=MAX_CACHE_SLOTS)
                .map(site)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let families: [(String, u8, Count, ValidationReason); 5] = [
            (
                sites(&|i| format!("if get_{i} then end")),
                Instr::OP_GET_GLOBAL,
                |bc| &mut bc.global_cache_slots,
                ValidationReason::GlobalCacheSlotOrder,
            ),
            (
                sites(&|i| format!("set_{i} = 1")),
                Instr::OP_SET_GLOBAL,
                |bc| &mut bc.global_cache_slots,
                ValidationReason::GlobalCacheSlotOrder,
            ),
            (
                format!("local t = {{}}\n{}", sites(&|_| "if t.x then end".into())),
                Instr::OP_GET_FIELD,
                |bc| &mut bc.field_cache_slots,
                ValidationReason::CacheSlotOrder {
                    family: CacheFamily::Field,
                },
            ),
            (
                format!("local t = {{}}\n{}", sites(&|_| "t.x = 1".into())),
                Instr::OP_SET_FIELD,
                |bc| &mut bc.set_field_cache_slots,
                ValidationReason::CacheSlotOrder {
                    family: CacheFamily::SetField,
                },
            ),
            (
                format!(
                    "local t = {{}}\n{}",
                    sites(&|_| "for _ in pairs(t) do end".into())
                ),
                Instr::OP_TFOR_CALL,
                |bc| &mut bc.tfor_cursor_slots,
                ValidationReason::CacheSlotOrder {
                    family: CacheFamily::TforCursor,
                },
            ),
        ];

        for (source, opcode, count_of, alias_reason) in families {
            let mut bc = crate::compiler::compile(&source, None).expect("capacity source compiles");
            validate_bytecode(&bc).expect("compiler output at capacity verifies");
            assert_eq!(usize::from(*count_of(&mut bc)), MAX_CACHE_SLOTS);
            let sites: Vec<usize> = (0..bc.code.len())
                .filter(|pc| bc.code[*pc].opcode() == opcode)
                .collect();
            assert_eq!(sites.len(), MAX_CACHE_SLOTS + 1, "opcode {opcode}");
            for (expected, pc) in sites.iter().take(MAX_CACHE_SLOTS).enumerate() {
                assert_eq!(bc.code[*pc].cache_slot(), Some(expected as u8));
            }
            let overflow = sites[MAX_CACHE_SLOTS];
            assert_eq!(bc.code[overflow].cache_slot(), None);

            // Reusing the last cached site's slot violates sequential ownership.
            let mut aliased = bc.clone();
            aliased.code[overflow] = aliased.code[overflow].with_cache_slot(Some(254));
            assert_rejects(&aliased, alias_reason);

            // A declared count that disagrees with the operands is rejected,
            // in either direction.
            for declared in [MAX_CACHE_SLOTS as u8 - 1, 0] {
                let mut miscounted = bc.clone();
                *count_of(&mut miscounted) = declared;
                assert_rejects(&miscounted, ValidationReason::CacheSlotCountMismatch);
            }
        }
    }

    /// Two field sites sharing one cache slot would let a hit for `t.a`
    /// answer `t.b`; ownership, not bounds, is what admission proves.
    #[test]
    fn a_cache_slot_owns_one_site() {
        let mut bc = crate::compiler::compile("local t = {}; local a, b = t.a, t.b", None)
            .expect("fixture compiles");
        let fields: Vec<usize> = (0..bc.code.len())
            .filter(|pc| bc.code[*pc].opcode() == Instr::OP_GET_FIELD)
            .collect();
        assert_eq!(fields.len(), 2);
        // Leave the compiled count unchanged so slot reuse is the only defect.
        bc.code[fields[1]] = bc.code[fields[1]].with_cache_slot(Some(0));
        assert_rejects(
            &bc,
            ValidationReason::CacheSlotOrder {
                family: CacheFamily::Field,
            },
        );
    }

    /// The typed operands render the sentences the verifier carried as
    /// strings before they were typed.
    #[test]
    fn typed_operands_keep_their_sentences() {
        use super::{BytecodeOperand, BytecodePool};
        for (reason, text) in [
            (
                ValidationReason::IndexOutOfRange {
                    operand: BytecodeOperand::NumberLiteral,
                },
                "number literal index is out of range",
            ),
            (
                ValidationReason::IndexOutOfRange {
                    operand: BytecodeOperand::StringLiteral,
                },
                "string literal index is out of range",
            ),
            (
                ValidationReason::IndexOutOfRange {
                    operand: BytecodeOperand::TableTemplate,
                },
                "table template index is out of range",
            ),
            (
                ValidationReason::IndexOutOfRange {
                    operand: BytecodeOperand::NestedChunk,
                },
                "nested chunk index is out of range",
            ),
            (
                ValidationReason::IndexOutOfRange {
                    operand: BytecodeOperand::Upvalue,
                },
                "upvalue index is out of range",
            ),
            (
                ValidationReason::TooMany {
                    what: BytecodePool::TableTemplate,
                    limit: 3,
                },
                "too many table templates (limit 3)",
            ),
            (
                ValidationReason::TooMany {
                    what: BytecodePool::NestedChunk,
                    limit: 3,
                },
                "too many nested chunks (limit 3)",
            ),
            (
                ValidationReason::TooMany {
                    what: BytecodePool::UpvalueDescriptor,
                    limit: 3,
                },
                "too many upvalue descriptors (limit 3)",
            ),
            (
                ValidationReason::TooMany {
                    what: BytecodePool::Literal,
                    limit: 3,
                },
                "too many literals (limit 3)",
            ),
            (
                ValidationReason::CacheSlotOrder {
                    family: CacheFamily::Field,
                },
                "field cache slot does not match instruction order",
            ),
            (
                ValidationReason::CacheSlotOrder {
                    family: CacheFamily::SetField,
                },
                "set-field cache slot does not match instruction order",
            ),
            (
                ValidationReason::CacheSlotOrder {
                    family: CacheFamily::TforCursor,
                },
                "TFOR_CALL cursor cache slot does not match instruction order",
            ),
        ] {
            assert_eq!(reason.to_string(), text);
        }
    }

    #[test]
    fn chunk_depth_includes_the_root() {
        let limit = crate::limits::MAX_CHUNK_NESTING;
        assert!(validate_depth(1).is_ok());
        assert!(validate_depth(limit).is_ok());
        assert!(validate_depth(limit + 1).is_err());
    }
}
