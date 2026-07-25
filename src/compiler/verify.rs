use super::Bytecode;
use crate::instr::{Builtin, Instr};

#[derive(Debug)]
pub(crate) struct BytecodeValidationError {
    #[cfg_attr(not(feature = "snapshot"), allow(dead_code))]
    pub(crate) instruction: Option<u32>,
    #[cfg_attr(not(feature = "snapshot"), allow(dead_code))]
    pub(crate) reason: String,
}

pub(crate) trait BytecodeView {
    fn code_len(&self) -> usize;
    fn raw_instruction(&self, pc: usize) -> u32;
    fn number_literals_len(&self) -> usize;
    fn string_literals(&self) -> &[Vec<u8>];
    fn table_templates(&self) -> &[Vec<u16>];
    fn global_cache_slots(&self) -> u8;
    fn field_cache_slots(&self) -> u8;
    fn set_field_cache_slots(&self) -> u8;
    fn num_params(&self) -> u8;
    fn num_locals(&self) -> u8;
    fn nested_len(&self) -> usize;
    fn upvalues_len(&self) -> usize;
    fn is_vararg(&self) -> bool;
    fn line_info_len(&self) -> usize;
}

impl BytecodeView for Bytecode {
    fn code_len(&self) -> usize {
        self.code.len()
    }
    fn raw_instruction(&self, pc: usize) -> u32 {
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
    fn num_params(&self) -> u8 {
        self.num_params
    }
    fn num_locals(&self) -> u8 {
        self.num_locals
    }
    fn nested_len(&self) -> usize {
        self.nested.len()
    }
    fn upvalues_len(&self) -> usize {
        self.upvalues.len()
    }
    fn is_vararg(&self) -> bool {
        self.is_vararg
    }
    fn line_info_len(&self) -> usize {
        self.line_info.len()
    }
}

fn err(pc: Option<usize>, reason: impl Into<String>) -> BytecodeValidationError {
    BytecodeValidationError {
        instruction: pc.map(|pc| pc as u32),
        reason: reason.into(),
    }
}

fn check_index(
    pc: usize,
    value: u16,
    len: usize,
    what: &str,
) -> Result<(), BytecodeValidationError> {
    if (value as usize) < len {
        Ok(())
    } else {
        Err(err(Some(pc), format!("{what} index is out of range")))
    }
}

fn check_slots(
    pc: usize,
    start: u8,
    count: usize,
    slots: usize,
) -> Result<(), BytecodeValidationError> {
    if (start as usize)
        .checked_add(count)
        .is_some_and(|end| end <= slots)
    {
        Ok(())
    } else {
        Err(err(Some(pc), "frame slot range is out of range"))
    }
}

fn known_opcode(opcode: u8) -> bool {
    matches!(
        opcode,
        Instr::OP_POP
            | Instr::OP_DUP
            | Instr::OP_SWAP
            | Instr::OP_NEW_TABLE
            | Instr::OP_GET_TABLE
            | Instr::OP_ADD
            | Instr::OP_SUBTRACT
            | Instr::OP_MULTIPLY
            | Instr::OP_DIVIDE
            | Instr::OP_POW
            | Instr::OP_MOD
            | Instr::OP_CONCAT
            | Instr::OP_LESS
            | Instr::OP_LESS_EQUAL
            | Instr::OP_GREATER
            | Instr::OP_GREATER_EQUAL
            | Instr::OP_EQUAL
            | Instr::OP_NOT_EQUAL
            | Instr::OP_NOT
            | Instr::OP_LENGTH
            | Instr::OP_NEGATE
            | Instr::OP_MARK_CALL_BASE
            | Instr::OP_PUSH_NIL
            | Instr::OP_NEW_TABLE_PRESIZED
            | Instr::OP_NEW_TABLE_TEMPLATE
            | Instr::OP_NEW_TABLE_TRACKED
            | Instr::OP_GET_GLOBAL
            | Instr::OP_SET_GLOBAL
            | Instr::OP_GET_LOCAL
            | Instr::OP_SET_LOCAL
            | Instr::OP_GET_UPVALUE
            | Instr::OP_SET_UPVALUE
            | Instr::OP_GET_FIELD
            | Instr::OP_INIT_INDEX
            | Instr::OP_PUSH_NUM
            | Instr::OP_PUSH_STRING
            | Instr::OP_TFOR_PREP
            | Instr::OP_RETURN
            | Instr::OP_CLOSE_UPVALUES
            | Instr::OP_CLOSURE
            | Instr::OP_VARARG
            | Instr::OP_SET_LIST
            | Instr::OP_SET_TABLE
            | Instr::OP_PUSH_BOOL
            | Instr::OP_GET_BUILTIN
            | Instr::OP_SET_BUILTIN
            | Instr::OP_SET_FIELD
            | Instr::OP_INIT_FIELD
            | Instr::OP_TFOR_CALL
            | Instr::OP_CALL
            | Instr::OP_INIT_FIELD_PINNED
            | Instr::OP_SET_FIELD_AT
            | Instr::OP_JUMP
            | Instr::OP_BRANCH_FALSE
            | Instr::OP_BRANCH_TRUE_KEEP
            | Instr::OP_BRANCH_FALSE_KEEP
            | Instr::OP_FOR_PREP
            | Instr::OP_FOR_LOOP
            | Instr::OP_TFOR_LOOP
    )
}

pub(crate) fn validate_bytecode(view: &impl BytecodeView) -> Result<(), BytecodeValidationError> {
    let code_len = view.code_len();
    if code_len == 0 {
        return Err(err(None, "code is empty"));
    }
    if view.line_info_len() != code_len {
        return Err(err(None, "line info length does not match code length"));
    }
    for (name, len) in [
        ("table template", view.table_templates().len()),
        ("nested chunk", view.nested_len()),
        ("upvalue descriptor", view.upvalues_len()),
    ] {
        if len > 255 {
            return Err(err(None, format!("too many {name}s")));
        }
    }
    if view.number_literals_len() > usize::from(u16::MAX) + 1
        || view.string_literals().len() > usize::from(u16::MAX) + 1
    {
        return Err(err(None, "too many literals"));
    }
    if view
        .table_templates()
        .iter()
        .any(|template| template.len() > 255)
    {
        return Err(err(None, "table template is too large"));
    }
    if view.num_params() as usize + view.num_locals() as usize > 255 {
        return Err(err(None, "frame slot count exceeds 255"));
    }
    if !matches!(
        Instr::from_raw(view.raw_instruction(code_len - 1)).opcode(),
        Instr::OP_RETURN
    ) {
        return Err(err(Some(code_len - 1), "code does not end in return"));
    }
    for (template_idx, template) in view.table_templates().iter().enumerate() {
        for key in template {
            if (*key as usize) >= view.string_literals().len() {
                return Err(err(
                    None,
                    format!("table template {template_idx} has an invalid string key"),
                ));
            }
        }
    }
    let slots = view.num_params() as usize + view.num_locals() as usize;
    let mut global_slots = vec![None; view.string_literals().len()];
    let mut next_global_slot = 0u8;
    let mut next_field_slot = 0u8;
    let mut next_set_field_slot = 0usize;
    for pc in 0..code_len {
        let inst = Instr::from_raw(view.raw_instruction(pc));
        let op = inst.opcode();
        if !known_opcode(op) {
            return Err(err(Some(pc), "unknown opcode"));
        }
        let a = inst.a();
        let b = inst.b();
        let c = inst.c();
        let bx = inst.bx();
        let reserved_zero = match op {
            // These forms use every operand byte.
            Instr::OP_GET_GLOBAL
            | Instr::OP_GET_FIELD
            | Instr::OP_SET_FIELD
            | Instr::OP_SET_FIELD_AT
            | Instr::OP_INIT_FIELD
            | Instr::OP_INIT_FIELD_PINNED
            | Instr::OP_SET_LIST
            | Instr::OP_FOR_PREP
            | Instr::OP_FOR_LOOP
            | Instr::OP_TFOR_LOOP => true,
            // Literal loads put the pool id in Bx and reserve A; jumps use B+C
            // as a signed offset and likewise reserve A.
            Instr::OP_PUSH_NUM
            | Instr::OP_PUSH_STRING
            | Instr::OP_SET_GLOBAL
            | Instr::OP_JUMP
            | Instr::OP_BRANCH_FALSE
            | Instr::OP_BRANCH_TRUE_KEEP
            | Instr::OP_BRANCH_FALSE_KEEP => a == 0,
            // Two-byte forms reserve C.
            Instr::OP_TFOR_CALL | Instr::OP_CALL => c == 0,
            // All remaining known instructions are either operandless or use
            // only A, and therefore reserve B+C (and A for operandless ops).
            _ => {
                let a_is_reserved = matches!(
                    op,
                    Instr::OP_POP
                        | Instr::OP_DUP
                        | Instr::OP_SWAP
                        | Instr::OP_NEW_TABLE
                        | Instr::OP_GET_TABLE
                        | Instr::OP_ADD
                        | Instr::OP_SUBTRACT
                        | Instr::OP_MULTIPLY
                        | Instr::OP_DIVIDE
                        | Instr::OP_POW
                        | Instr::OP_MOD
                        | Instr::OP_LESS
                        | Instr::OP_LESS_EQUAL
                        | Instr::OP_GREATER
                        | Instr::OP_GREATER_EQUAL
                        | Instr::OP_EQUAL
                        | Instr::OP_NOT_EQUAL
                        | Instr::OP_NOT
                        | Instr::OP_LENGTH
                        | Instr::OP_NEGATE
                        | Instr::OP_PUSH_NIL
                );
                (!a_is_reserved || a == 0) && b == 0 && c == 0
            }
        };
        if !reserved_zero {
            return Err(err(
                Some(pc),
                format!(
                    "reserved operand bytes are non-zero (opcode {op}, raw {:08x})",
                    inst.raw()
                ),
            ));
        }
        match op {
            Instr::OP_PUSH_NUM => {
                check_index(pc, bx, view.number_literals_len(), "number literal")?;
            }
            Instr::OP_GET_GLOBAL | Instr::OP_SET_GLOBAL => {
                check_index(pc, bx, view.string_literals().len(), "string literal")?;
                if std::str::from_utf8(&view.string_literals()[bx as usize]).is_err() {
                    return Err(err(Some(pc), "global name operand is not UTF-8"));
                }
                // The sentinel means "deliberately uncached" and is valid
                // anywhere, not only once the slots are exhausted. The compiler
                // never emits it early, but a hand-built or deliberately
                // uncached snapshot is still semantically valid, and the
                // runtime handles it by taking the slow path.
                if op == Instr::OP_GET_GLOBAL && a != u8::MAX {
                    let expected = match global_slots[bx as usize] {
                        Some(slot) => slot,
                        None => {
                            let slot = next_global_slot;
                            if slot != u8::MAX {
                                next_global_slot += 1;
                            }
                            global_slots[bx as usize] = Some(slot);
                            slot
                        }
                    };
                    if a != expected {
                        return Err(err(
                            Some(pc),
                            "global cache slot does not match first-use order",
                        ));
                    }
                }
            }
            Instr::OP_GET_FIELD => {
                check_index(pc, bx, view.string_literals().len(), "string literal")?;
                // As above: the sentinel is accepted anywhere and excluded from
                // the sequential slot count.
                if a != u8::MAX {
                    if a != next_field_slot {
                        return Err(err(
                            Some(pc),
                            "field cache slot does not match instruction order",
                        ));
                    }
                    next_field_slot += 1;
                }
            }
            Instr::OP_SET_FIELD => {
                check_index(pc, bx, view.string_literals().len(), "string literal")?;
                if a != u8::MAX && a as usize != next_set_field_slot {
                    return Err(err(
                        Some(pc),
                        "set-field cache slot does not match instruction order",
                    ));
                }
                if a != u8::MAX {
                    next_set_field_slot += 1;
                }
            }
            // Every remaining opcode that names a string literal in Bx. Merged
            // into one arm so the bounds check cannot be forgotten for one of
            // them; forged bytecode reaches this path too.
            Instr::OP_PUSH_STRING
            | Instr::OP_INIT_FIELD_PINNED
            | Instr::OP_SET_FIELD_AT
            | Instr::OP_INIT_FIELD => {
                check_index(pc, bx, view.string_literals().len(), "string literal")?;
            }
            Instr::OP_NEW_TABLE_TEMPLATE => {
                check_index(
                    pc,
                    u16::from(a),
                    view.table_templates().len(),
                    "table template",
                )?;
            }
            Instr::OP_CLOSURE => check_index(pc, u16::from(a), view.nested_len(), "nested chunk")?,
            Instr::OP_GET_BUILTIN | Instr::OP_SET_BUILTIN if (a as usize) >= Builtin::COUNT => {
                return Err(err(Some(pc), "builtin slot is out of range"));
            }
            Instr::OP_GET_LOCAL | Instr::OP_SET_LOCAL => check_slots(pc, a, 1, slots)?,
            Instr::OP_GET_UPVALUE | Instr::OP_SET_UPVALUE => {
                check_index(pc, u16::from(a), view.upvalues_len(), "upvalue")?;
            }
            Instr::OP_FOR_PREP | Instr::OP_FOR_LOOP | Instr::OP_TFOR_LOOP => {
                check_slots(pc, a, 4, slots)?;
            }
            Instr::OP_TFOR_PREP => check_slots(pc, a, 3, slots)?,
            Instr::OP_TFOR_CALL => {
                if b == 0 {
                    return Err(err(Some(pc), "TFOR_CALL requires at least one result"));
                }
                check_slots(pc, a, 3 + b as usize, slots)?;
            }
            Instr::OP_CLOSE_UPVALUES if (a as usize) > slots => {
                return Err(err(Some(pc), "close-upvalues level is out of range"));
            }
            Instr::OP_VARARG if !view.is_vararg() => {
                return Err(err(Some(pc), "vararg instruction in non-vararg function"));
            }
            Instr::OP_PUSH_BOOL if a > 1 => {
                return Err(err(Some(pc), "boolean operand is invalid"));
            }
            Instr::OP_CONCAT if a < 2 => {
                return Err(err(Some(pc), "concat requires at least two values"));
            }
            // Always 1: the marker is emitted after the callee is evaluated, so
            // the callee is the single value on top and the base is one slot
            // below it. Anything else is corrupt.
            Instr::OP_MARK_CALL_BASE if a != 1 => {
                return Err(err(Some(pc), "call-base marker adjustment is invalid"));
            }
            _ => {}
        }
        if matches!(
            op,
            Instr::OP_JUMP
                | Instr::OP_BRANCH_FALSE
                | Instr::OP_BRANCH_TRUE_KEEP
                | Instr::OP_BRANCH_FALSE_KEEP
                | Instr::OP_FOR_PREP
                | Instr::OP_FOR_LOOP
                | Instr::OP_TFOR_LOOP
        ) {
            let target = match i64::try_from(pc) {
                Ok(pc) => pc.checked_add(1),
                Err(_) => None,
            }
            .and_then(|next| next.checked_add(inst.sbx() as i64));
            if !target.is_some_and(|target| target >= 0 && target < code_len as i64) {
                return Err(err(Some(pc), "jump target is out of range"));
            }
        }
    }
    if view.global_cache_slots() != next_global_slot
        || view.field_cache_slots() != next_field_slot
        || view.set_field_cache_slots() as usize != next_set_field_slot
    {
        return Err(err(None, "declared cache slots do not match instructions"));
    }
    Ok(())
}
