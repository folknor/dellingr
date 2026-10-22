use super::{Bytecode, UpvalueDesc};
use crate::instr::{ArgCount, Builtin, Instr, RetCount};
use crate::limits::{
    MAX_FRAME_SLOTS, MAX_LITERALS, MAX_NESTED, MAX_TABLE_TEMPLATE_FIELDS, MAX_TABLE_TEMPLATES,
    MAX_UPVALUES, MAX_VERIFIER_MARKER_DEPTH,
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Height {
    Known(u32),
    Dyn { floor: u32 },
}

impl Height {
    fn floor(&self) -> u32 {
        match self {
            Self::Known(height) | Self::Dyn { floor: height } => *height,
        }
    }

    fn known(&self) -> Option<u32> {
        match self {
            Self::Known(height) => Some(*height),
            Self::Dyn { .. } => None,
        }
    }

    fn add(&mut self, amount: u32, pc: usize) -> Result<(), BytecodeValidationError> {
        match self {
            Self::Known(height) => {
                *height = height
                    .checked_add(amount)
                    .ok_or_else(|| err(Some(pc), "operand stack height overflows"))?;
            }
            Self::Dyn { floor } => {
                *floor = floor
                    .checked_add(amount)
                    .ok_or_else(|| err(Some(pc), "operand stack height overflows"))?;
            }
        }
        Ok(())
    }

    fn remove(&mut self, amount: u32, pc: usize) -> Result<(), BytecodeValidationError> {
        if self.floor() < amount {
            return Err(err(Some(pc), "operand stack underflows"));
        }
        match self {
            Self::Known(height) => {
                *height = height
                    .checked_sub(amount)
                    .ok_or_else(|| err(Some(pc), "operand stack underflows"))?;
            }
            Self::Dyn { floor } => {
                *floor = floor
                    .checked_sub(amount)
                    .ok_or_else(|| err(Some(pc), "operand stack underflows"))?;
            }
        }
        Ok(())
    }

    fn adjust(
        &mut self,
        removed: u32,
        added: u32,
        pc: usize,
    ) -> Result<(), BytecodeValidationError> {
        self.remove(removed, pc)?;
        self.add(added, pc)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AbsState {
    height: Height,
    call_bases: Vec<u32>,
    table_bases: Vec<u32>,
}

#[derive(Debug)]
pub(crate) struct BytecodeValidationError {
    pub(crate) instruction: Option<u32>,
    pub(crate) reason: String,
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
            return Err(err(None, "child upvalue descriptor is invalid for parent"));
        }
    }
    Ok(())
}

pub(crate) fn validate_depth(depth: usize) -> Result<(), BytecodeValidationError> {
    if depth > crate::limits::MAX_CHUNK_NESTING {
        return Err(err(None, "nested chunk depth exceeds parser limit"));
    }
    Ok(())
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
    fn tfor_cursor_slots(&self) -> u8;
    fn num_params(&self) -> u8;
    fn num_locals(&self) -> u8;
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
    fn tfor_cursor_slots(&self) -> u8 {
        self.tfor_cursor_slots
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

fn verify_stack_discipline(view: &impl BytecodeView) -> Result<(), BytecodeValidationError> {
    // Abstract states are stored only at join candidates: transfer targets and
    // the entry. Any other pc has exactly one predecessor (the instruction
    // before it), which executes at most once, so its state travels in the
    // worklist by value and is never compared. Storing a state per reachable
    // pc instead cost a marker-stack clone per instruction. The worklist order,
    // the comparison points and therefore the first reported error are the
    // same as storing everywhere.
    let code_len = view.code_len();
    // Dense ordinal of each join candidate; `u32::MAX` marks a plain pc.
    let mut join_index = vec![u32::MAX; code_len];
    let mut join_count = 0u32;
    let mut mark_join = |pc: usize| {
        if join_index[pc] == u32::MAX {
            join_index[pc] = join_count;
            join_count += 1;
        }
    };
    mark_join(0);
    for pc in 0..code_len {
        let inst = Instr::from_raw(view.raw_instruction(pc));
        if Instr::is_transfer(inst.opcode()) {
            // `validate_bytecode` range-checked every transfer target before
            // calling this, so the target is a valid pc.
            let target = (pc as i64 + 1 + i64::from(inst.sbx())) as usize;
            mark_join(target);
        }
    }
    let entry = AbsState {
        height: Height::Known(0),
        call_bases: Vec::new(),
        table_bases: Vec::new(),
    };
    let mut joins: Vec<Option<AbsState>> = vec![None; join_count as usize];
    joins[join_index[0] as usize] = Some(entry.clone());
    let mut worklist = vec![(0usize, entry)];

    while let Some((pc, state)) = worklist.pop() {
        let inst = Instr::from_raw(view.raw_instruction(pc));
        let op = inst.opcode();
        let a = inst.a();
        let mut next = state;

        let require_known = |height: &Height| {
            height.known().ok_or_else(|| {
                err(
                    Some(pc),
                    "control flow requires a known operand stack height",
                )
            })
        };
        let require_at_least = |height: &Height, amount: u32| {
            if height.floor() < amount {
                Err(err(Some(pc), "operand stack underflows"))
            } else {
                Ok(())
            }
        };
        let offset_requirement = |offset: u8, extra: u32| {
            u32::from(offset)
                .checked_add(extra)
                .ok_or_else(|| err(Some(pc), "operand stack height overflows"))
        };

        // Stack semantics stay explicit: dynamic calls, list constructors and
        // marker stacks change abstract state beyond a fixed pop/push count.
        // Operand encoding and transfer membership come from Instr metadata.
        match op {
            Instr::OP_DUP => {
                require_at_least(&next.height, 1)?;
                next.height.add(1, pc)?;
            }
            Instr::OP_SWAP => require_at_least(&next.height, 2)?,
            Instr::OP_NEW_TABLE
            | Instr::OP_NEW_TABLE_PRESIZED
            | Instr::OP_NEW_TABLE_TEMPLATE
            | Instr::OP_PUSH_NIL
            | Instr::OP_PUSH_BOOL
            | Instr::OP_PUSH_NUM
            | Instr::OP_PUSH_STRING
            | Instr::OP_GET_GLOBAL
            | Instr::OP_GET_LOCAL
            | Instr::OP_GET_UPVALUE
            | Instr::OP_GET_BUILTIN
            | Instr::OP_CLOSURE => next.height.add(1, pc)?,
            // `GET_TABLE` pops the key and replaces the table with the result,
            // which is the same two-in one-out shape as a binary operator.
            Instr::OP_GET_TABLE
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
            | Instr::OP_NOT_EQUAL => next.height.adjust(2, 1, pc)?,
            Instr::OP_CONCAT => next.height.adjust(u32::from(a), 1, pc)?,
            Instr::OP_NOT | Instr::OP_LENGTH | Instr::OP_NEGATE | Instr::OP_GET_FIELD => {
                require_at_least(&next.height, 1)?;
            }
            Instr::OP_MARK_CALL_BASE => {
                let height = require_known(&next.height)?;
                if height < 1 {
                    return Err(err(Some(pc), "call-base marker is below the active frame"));
                }
                if next.call_bases.len() >= MAX_VERIFIER_MARKER_DEPTH {
                    return Err(err(Some(pc), "call-base marker stack is too deep"));
                }
                next.call_bases.push(height - 1);
            }
            Instr::OP_NEW_TABLE_TRACKED => {
                let height = require_known(&next.height)?;
                if next.table_bases.len() >= MAX_VERIFIER_MARKER_DEPTH {
                    return Err(err(Some(pc), "table-constructor marker stack is too deep"));
                }
                next.table_bases.push(height);
                next.height.add(1, pc)?;
            }
            // A discard and a store share one operand effect: consume the top
            // value and push nothing. The store's destination is a local,
            // upvalue or global slot, none of which is on the operand stack.
            Instr::OP_POP
            | Instr::OP_SET_GLOBAL
            | Instr::OP_SET_LOCAL
            | Instr::OP_SET_UPVALUE
            | Instr::OP_SET_BUILTIN => next.height.remove(1, pc)?,
            Instr::OP_INIT_FIELD => {
                require_at_least(&next.height, offset_requirement(a, 2)?)?;
                next.height.remove(1, pc)?;
            }
            Instr::OP_INIT_FIELD_PINNED => {
                require_at_least(&next.height, 2)?;
                next.height.remove(1, pc)?;
            }
            Instr::OP_INIT_INDEX => {
                require_at_least(&next.height, offset_requirement(a, 3)?)?;
                next.height.remove(2, pc)?;
            }
            Instr::OP_SET_FIELD => {
                require_at_least(&next.height, 2)?;
                next.height.remove(2, pc)?;
            }
            Instr::OP_SET_FIELD_AT => {
                // A=0 is valid for generic bytecode even though compiler output
                // rewrites that form to the cached field-store opcode.
                require_at_least(&next.height, offset_requirement(a, 2)?)?;
                next.height.remove(2, pc)?;
            }
            Instr::OP_SET_TABLE => {
                require_at_least(&next.height, offset_requirement(a, 3)?)?;
                next.height.remove(3, pc)?;
            }
            Instr::OP_SET_LIST if a == 0 => {
                let base = match next.table_bases.pop() {
                    Some(base) => base,
                    None => return Err(err(Some(pc), "dynamic set-list has no table marker")),
                };
                let minimum = base
                    .checked_add(1)
                    .ok_or_else(|| err(Some(pc), "operand stack height overflows"))?;
                require_at_least(&next.height, minimum)?;
                next.height = Height::Known(minimum);
            }
            Instr::OP_SET_LIST => {
                let count = u32::from(a);
                require_at_least(
                    &next.height,
                    count
                        .checked_add(1)
                        .ok_or_else(|| err(Some(pc), "operand stack height overflows"))?,
                )?;
                next.height.remove(count, pc)?;
            }
            Instr::OP_VARARG => match inst.vararg_count() {
                RetCount::All => {
                    let floor = next.height.floor();
                    next.height = Height::Dyn { floor };
                }
                RetCount::Fixed(n) => next.height.add(u32::from(n), pc)?,
            },
            Instr::OP_CALL => match (inst.call_args(), inst.call_results()) {
                (ArgCount::Dynamic, results) => {
                    let base = match next.call_bases.pop() {
                        Some(base) => base,
                        None => return Err(err(Some(pc), "dynamic call has no call-base marker")),
                    };
                    let minimum = base
                        .checked_add(1)
                        .ok_or_else(|| err(Some(pc), "operand stack height overflows"))?;
                    require_at_least(&next.height, minimum)?;
                    next.height = match results {
                        RetCount::All => Height::Dyn { floor: base },
                        RetCount::Fixed(n) => Height::Known(
                            base.checked_add(u32::from(n))
                                .ok_or_else(|| err(Some(pc), "operand stack height overflows"))?,
                        ),
                    };
                }
                (ArgCount::Fixed(args), results) => {
                    let inputs = u32::from(args)
                        .checked_add(1)
                        .ok_or_else(|| err(Some(pc), "operand stack height overflows"))?;
                    match results {
                        RetCount::All => {
                            next.height.remove(inputs, pc)?;
                            let floor = next.height.floor();
                            next.height = Height::Dyn { floor };
                        }
                        RetCount::Fixed(n) => next.height.adjust(inputs, u32::from(n), pc)?,
                    }
                }
            },
            Instr::OP_TFOR_PREP => next.height.remove(3, pc)?,
            // Both work entirely within the locals region: `TFOR_CALL` drives
            // the iterator through local slots and is internally balanced, and
            // `CLOSE_UPVALUES` only closes upvalues at or above a local slot.
            Instr::OP_NOP | Instr::OP_TFOR_CALL | Instr::OP_CLOSE_UPVALUES => {}
            Instr::OP_FOR_PREP => {
                require_known(&next.height)?;
                next.height.remove(3, pc)?;
            }
            Instr::OP_FOR_LOOP | Instr::OP_TFOR_LOOP | Instr::OP_JUMP => {
                require_known(&next.height)?;
            }
            Instr::OP_BRANCH_FALSE => {
                require_known(&next.height)?;
                next.height.remove(1, pc)?;
            }
            Instr::OP_BRANCH_FALSE_LESS
            | Instr::OP_BRANCH_FALSE_LESS_EQUAL
            | Instr::OP_BRANCH_FALSE_GREATER
            | Instr::OP_BRANCH_FALSE_GREATER_EQUAL
            | Instr::OP_BRANCH_FALSE_EQUAL
            | Instr::OP_BRANCH_FALSE_NOT_EQUAL => {
                require_known(&next.height)?;
                next.height.remove(2, pc)?;
            }
            Instr::OP_BRANCH_TRUE_KEEP | Instr::OP_BRANCH_FALSE_KEEP => {
                require_known(&next.height)?;
                require_at_least(&next.height, 1)?;
            }
            Instr::OP_RETURN => {
                if !next.call_bases.is_empty() || !next.table_bases.is_empty() {
                    return Err(err(Some(pc), "return leaves a live stack marker"));
                }
                if let RetCount::Fixed(n) = inst.return_count() {
                    let height = require_known(&next.height)?;
                    if height < u32::from(n) {
                        return Err(err(Some(pc), "return values exceed operand stack height"));
                    }
                }
            }
            _ => return Err(err(Some(pc), "missing operand stack effect")),
        }

        for base in next.call_bases.iter().chain(next.table_bases.iter()) {
            let minimum = base
                .checked_add(1)
                .ok_or_else(|| err(Some(pc), "operand stack height overflows"))?;
            if next.height.floor() < minimum {
                return Err(err(Some(pc), "operand stack drops below a live marker"));
            }
        }

        if op == Instr::OP_RETURN {
            continue;
        }
        let target = |inst: Instr| -> Result<usize, BytecodeValidationError> {
            let next_pc = pc
                .checked_add(1)
                .ok_or_else(|| err(Some(pc), "jump target is out of range"))?;
            if inst.sbx() >= 0 {
                next_pc
                    .checked_add(inst.sbx() as usize)
                    .ok_or_else(|| err(Some(pc), "jump target is out of range"))
            } else {
                next_pc
                    .checked_sub(inst.sbx().unsigned_abs() as usize)
                    .ok_or_else(|| err(Some(pc), "jump target is out of range"))
            }
        };
        let fallthrough = (op != Instr::OP_JUMP).then_some(pc + 1);
        let jump = if Instr::is_transfer(op) {
            Some(target(inst)?)
        } else {
            None
        };
        // Compare at enumeration time, in successor order, exactly where a
        // per-pc state table would: that fixes which error is reported first.
        let mut to_push = [None; 2];
        let mut pushes = 0;
        for successor in [fallthrough, jump].into_iter().flatten() {
            let join = join_index[successor];
            if join == u32::MAX {
                // Only a fallthrough can land on a plain pc, and that pc's sole
                // predecessor is this one: first and only arrival.
                to_push[pushes] = Some(successor);
                pushes += 1;
                continue;
            }
            match &joins[join as usize] {
                Some(existing) if existing != &next => {
                    return Err(err(
                        Some(successor),
                        "operand stack state disagrees at control-flow join",
                    ));
                }
                Some(_) => {}
                None => {
                    joins[join as usize] = Some(next.clone());
                    to_push[pushes] = Some(successor);
                    pushes += 1;
                }
            }
        }
        match to_push {
            [Some(first), Some(second)] => {
                worklist.push((first, next.clone()));
                worklist.push((second, next));
            }
            [Some(only), None] => worklist.push((only, next)),
            _ => {}
        }
    }
    Ok(())
}

// Dispatch policy: verify static operand bounds, stack shape, marker pairing
// and control flow here at admission, then trust them during execution. This
// does not prove runtime value types: constructor operands, numeric loop values,
// callable handlers and host/resource limits still need fallible runtime checks.
// A regression in these static proofs is a VM bug, not a recoverable Lua error.
// The verifier also accepts valid caller-built and snapshot bytecode forms
// that compiler finalization normalizes: it strips `OP_NOP` and rewrites
// `OP_SET_FIELD_AT` with A=0 to a cached `OP_SET_FIELD`.
pub(crate) fn validate_bytecode(view: &impl BytecodeView) -> Result<(), BytecodeValidationError> {
    let code_len = view.code_len();
    if code_len == 0 {
        return Err(err(None, "code is empty"));
    }
    if view.line_info_len() != code_len {
        return Err(err(None, "line info length does not match code length"));
    }
    for (name, len, limit) in [
        (
            "table template",
            view.table_templates().len(),
            MAX_TABLE_TEMPLATES,
        ),
        ("nested chunk", view.nested_len(), MAX_NESTED),
        ("upvalue descriptor", view.upvalues_len(), MAX_UPVALUES),
    ] {
        if len > limit {
            return Err(err(None, format!("too many {name}s (limit {limit})")));
        }
    }
    if view.number_literals_len() > MAX_LITERALS || view.string_literals().len() > MAX_LITERALS {
        return Err(err(
            None,
            format!("too many literals (limit {MAX_LITERALS})"),
        ));
    }
    if view
        .table_templates()
        .iter()
        .any(|template| template.len() > MAX_TABLE_TEMPLATE_FIELDS)
    {
        return Err(err(
            None,
            format!("table template exceeds limit {MAX_TABLE_TEMPLATE_FIELDS}"),
        ));
    }
    if view.num_params() as usize + view.num_locals() as usize > MAX_FRAME_SLOTS {
        return Err(err(
            None,
            format!("frame slot count exceeds {MAX_FRAME_SLOTS}"),
        ));
    }
    if !matches!(
        Instr::from_raw(view.raw_instruction(code_len - 1)).opcode(),
        Instr::OP_RETURN
    ) {
        return Err(err(Some(code_len - 1), "code does not end in return"));
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
                    format!("table template {template_idx} has an invalid string key"),
                ));
            };
            if !seen.insert(bytes.as_slice()) {
                return Err(err(
                    None,
                    format!("table template {template_idx} has a duplicate key"),
                ));
            }
        }
    }
    let slots = view.num_params() as usize + view.num_locals() as usize;
    let mut global_slots: Vec<Option<usize>> = vec![None; view.string_literals().len()];
    let mut global_name_checked = vec![false; view.string_literals().len()];
    let mut next_global_slot = 0usize;
    let mut next_field_slot = 0usize;
    let mut next_set_field_slot = 0usize;
    let mut next_tfor_cursor_slot = 0usize;
    // This loop deliberately restates the cache-slot allocation policy of
    // `Bytecode::assign_cache_slots` (first-use order for globals, a fresh
    // global slot per SET_GLOBAL, instruction order for fields, set-fields and
    // TFOR_CALL cursors). The duplication is the point: the verifier is the
    // independent check on a snapshot's cached operands, and a shared helper
    // would let a policy bug validate itself. Any change to the allocation
    // policy must change both. Compiler finalization runs this validation on
    // every compile, so a drift between the two fails every compile that
    // reaches the changed case. The operand encoding itself is not restated:
    // both sides decode through the `Instr` cache-slot decoders.
    //
    // Uncached operands are accepted anywhere and consume no slot. Sequential
    // allocation plus the final count equality prove every admitted slot is
    // below its family's declared count, which the runtime cache vectors are
    // sized from.
    let sequential = |pc: usize,
                      slot: u8,
                      next: &mut usize,
                      what: &str|
     -> Result<(), BytecodeValidationError> {
        if usize::from(slot) != *next {
            return Err(err(
                Some(pc),
                format!("{what} cache slot does not match instruction order"),
            ));
        }
        *next += 1;
        Ok(())
    };
    for pc in 0..code_len {
        let inst = Instr::from_raw(view.raw_instruction(pc));
        let op = inst.opcode();
        if !Instr::known_opcode(op) {
            return Err(err(Some(pc), "unknown opcode"));
        }
        let a = inst.a();
        let b = inst.b();
        let bx = inst.bx();
        let reserved_zero = inst.reserved_bytes_zero();
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
                // Check each name once: re-scanning a long name at every
                // reference would cost references x name length. The first
                // reference to a bad name still reports.
                if !global_name_checked[bx as usize] {
                    if std::str::from_utf8(&view.string_literals()[bx as usize]).is_err() {
                        return Err(err(Some(pc), "global name operand is not UTF-8"));
                    }
                    global_name_checked[bx as usize] = true;
                }
                // "Uncached" is valid anywhere, not only once the slots are
                // exhausted. The compiler never emits it early, but a
                // hand-built or deliberately uncached snapshot is still
                // semantically valid, and the runtime takes the slow path.
                // An uncached read neither reserves a slot for its name nor
                // marks the name exhausted.
                let global_order_error =
                    || err(Some(pc), "global cache slot does not match first-use order");
                match (op, inst.global_cache_slot()) {
                    (_, None) => {}
                    (Instr::OP_GET_GLOBAL, Some(slot)) => {
                        // A cached read reuses its name's slot, or allocates
                        // the next one on the name's first cached read.
                        let expected = *global_slots[bx as usize].get_or_insert_with(|| {
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
            Instr::OP_GET_FIELD => {
                check_index(pc, bx, view.string_literals().len(), "string literal")?;
                if let Some(slot) = inst.field_cache_slot() {
                    sequential(pc, slot, &mut next_field_slot, "field")?;
                }
            }
            Instr::OP_SET_FIELD => {
                check_index(pc, bx, view.string_literals().len(), "string literal")?;
                if let Some(slot) = inst.set_field_cache_slot() {
                    sequential(pc, slot, &mut next_set_field_slot, "set-field")?;
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
                if let Some(slot) = inst.tfor_cursor_slot() {
                    sequential(pc, slot, &mut next_tfor_cursor_slot, "TFOR_CALL cursor")?;
                }
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
        if Instr::is_transfer(op) {
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
    if usize::from(view.global_cache_slots()) != next_global_slot
        || usize::from(view.field_cache_slots()) != next_field_slot
        || usize::from(view.set_field_cache_slots()) != next_set_field_slot
        || usize::from(view.tfor_cursor_slots()) != next_tfor_cursor_slot
    {
        return Err(err(None, "declared cache slots do not match instructions"));
    }
    verify_stack_discipline(view)
}

#[cfg(test)]
mod tree_tests {
    use super::{Bytecode, UpvalueDesc, validate_child, validate_depth};

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

    /// Each cache family filled past capacity: 255 cached sites, then an
    /// uncached one. The runtime sizes each cache vector from the declared
    /// count and looks slots up by decoded index, so admission must keep every
    /// cached slot below that count and the count within `MAX_CACHE_SLOTS`.
    #[test]
    fn cache_families_cap_below_the_encodable_limit() {
        use super::validate_bytecode;
        use crate::instr::{Instr, MAX_CACHE_SLOTS};

        type Slot = fn(Instr) -> Option<u8>;
        type Count = fn(&mut Bytecode) -> &mut u8;
        let sites = |site: &dyn Fn(usize) -> String| {
            (0..=MAX_CACHE_SLOTS)
                .map(site)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let families: [(String, u8, Slot, Count); 5] = [
            (
                sites(&|i| format!("if get_{i} then end")),
                Instr::OP_GET_GLOBAL,
                Instr::global_cache_slot,
                |bc| &mut bc.global_cache_slots,
            ),
            (
                sites(&|i| format!("set_{i} = 1")),
                Instr::OP_SET_GLOBAL,
                Instr::global_cache_slot,
                |bc| &mut bc.global_cache_slots,
            ),
            (
                format!("local t = {{}}\n{}", sites(&|_| "if t.x then end".into())),
                Instr::OP_GET_FIELD,
                Instr::field_cache_slot,
                |bc| &mut bc.field_cache_slots,
            ),
            (
                format!("local t = {{}}\n{}", sites(&|_| "t.x = 1".into())),
                Instr::OP_SET_FIELD,
                Instr::set_field_cache_slot,
                |bc| &mut bc.set_field_cache_slots,
            ),
            (
                format!(
                    "local t = {{}}\n{}",
                    sites(&|_| "for _ in pairs(t) do end".into())
                ),
                Instr::OP_TFOR_CALL,
                Instr::tfor_cursor_slot,
                |bc| &mut bc.tfor_cursor_slots,
            ),
        ];

        for (source, opcode, slot_of, count_of) in families {
            let mut bc = crate::compiler::parse_str(&source).expect("capacity source compiles");
            validate_bytecode(&bc).expect("compiler output at capacity verifies");
            assert_eq!(usize::from(*count_of(&mut bc)), MAX_CACHE_SLOTS);
            let sites: Vec<usize> = (0..bc.code.len())
                .filter(|pc| bc.code[*pc].opcode() == opcode)
                .collect();
            assert_eq!(sites.len(), MAX_CACHE_SLOTS + 1, "opcode {opcode}");
            for (expected, pc) in sites.iter().take(MAX_CACHE_SLOTS).enumerate() {
                assert_eq!(slot_of(bc.code[*pc]), Some(expected as u8));
            }
            let overflow = sites[MAX_CACHE_SLOTS];
            assert_eq!(slot_of(bc.code[overflow]), None);

            // Caching the overflow site as a repeat of the last slot is
            // rejected: it would alias slot 254 rather than fit a 256th.
            let mut aliased = bc.clone();
            let last = aliased.code[sites[MAX_CACHE_SLOTS - 1]];
            let mut raw = aliased.code[overflow].raw();
            raw = if opcode == Instr::OP_TFOR_CALL {
                (raw & !0xff) | (last.raw() & 0xff)
            } else {
                (raw & !0x00ff_0000) | (last.raw() & 0x00ff_0000)
            };
            aliased.code[overflow] = Instr::from_raw(raw);
            assert_eq!(slot_of(aliased.code[overflow]), Some(254));
            assert!(validate_bytecode(&aliased).is_err(), "opcode {opcode}");

            // A declared count that disagrees with the operands is rejected,
            // in either direction.
            for declared in [MAX_CACHE_SLOTS as u8 - 1, 0] {
                let mut miscounted = bc.clone();
                *count_of(&mut miscounted) = declared;
                assert!(validate_bytecode(&miscounted).is_err(), "opcode {opcode}");
            }
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
