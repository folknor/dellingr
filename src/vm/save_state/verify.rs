use std::collections::VecDeque;

use crate::compiler::MAX_SYNTAX_DEPTH;
use crate::compiler::verify::{self, BytecodeView};

use super::{LoadError, SavePayload, SavedBytecode, SavedObject, SavedUpvalueDesc};

/// A payload whose bytecode and closure references have been structurally checked.
///
/// This type intentionally has no constructor outside this module.
pub(super) struct VerifiedSavePayload(SavePayload);

impl VerifiedSavePayload {
    pub(super) fn has_standard_environment(&self) -> bool {
        self.0.has_standard_environment
    }
    pub(super) fn into_inner(self) -> SavePayload {
        self.0
    }
    pub(super) fn bytecode(&self) -> &[SavedBytecode] {
        &self.0.bytecode
    }
}

impl BytecodeView for SavedBytecode {
    fn code_len(&self) -> usize {
        self.code.len()
    }
    fn raw_instruction(&self, pc: usize) -> u32 {
        self.code[pc]
    }
    fn number_literals_len(&self) -> usize {
        self.number_literals.len()
    }
    fn string_literals(&self) -> &[Vec<u8>] {
        &self.string_literals
    }
    fn table_templates(&self) -> &[Vec<u8>] {
        &self.table_templates
    }
    fn global_cache_slots(&self) -> u16 {
        self.global_cache_slots
    }
    fn field_cache_slots(&self) -> u16 {
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

fn invalid(chunk: usize, instruction: Option<u32>, reason: impl Into<String>) -> LoadError {
    LoadError::InvalidBytecode {
        chunk: u32::try_from(chunk).unwrap_or(u32::MAX),
        instruction,
        reason: reason.into(),
    }
}

fn validate_nested_graph(bytecode: &[SavedBytecode]) -> Result<(), LoadError> {
    let mut indegree = vec![0usize; bytecode.len()];
    for (parent, chunk) in bytecode.iter().enumerate() {
        for child in &chunk.nested {
            let child = *child as usize;
            if child >= bytecode.len() {
                return Err(LoadError::CorruptArena);
            }
            indegree[child] = indegree[child]
                .checked_add(1)
                .ok_or_else(|| invalid(parent, None, "nested graph is too large"))?;
            for descriptor in &bytecode[child].upvalues {
                let valid = match descriptor {
                    SavedUpvalueDesc::Local(slot) => {
                        (*slot as usize) < chunk.num_params as usize + chunk.num_locals as usize
                    }
                    SavedUpvalueDesc::Upvalue(slot) => (*slot as usize) < chunk.upvalues.len(),
                };
                if !valid {
                    return Err(invalid(
                        parent,
                        None,
                        "child upvalue descriptor is invalid for parent",
                    ));
                }
            }
        }
    }
    let mut depths = vec![0u32; bytecode.len()];
    let mut ready = VecDeque::new();
    for (id, degree) in indegree.iter().enumerate() {
        if *degree == 0 {
            depths[id] = 1;
            ready.push_back(id);
        }
    }
    let mut visited = 0usize;
    while let Some(parent) = ready.pop_front() {
        visited += 1;
        for child in &bytecode[parent].nested {
            let child = *child as usize;
            let depth = depths[parent] + 1;
            depths[child] = depths[child].max(depth);
            if depths[child] > MAX_SYNTAX_DEPTH {
                return Err(invalid(
                    child,
                    None,
                    "nested chunk depth exceeds parser limit",
                ));
            }
            indegree[child] -= 1;
            if indegree[child] == 0 {
                ready.push_back(child);
            }
        }
    }
    if visited != bytecode.len() {
        return Err(invalid(0, None, "nested chunk graph contains a cycle"));
    }
    Ok(())
}

fn validate_closures(payload: &SavePayload) -> Result<(), LoadError> {
    for object in &payload.objects {
        let SavedObject::Closure { chunk, upvalues } = object else {
            continue;
        };
        let chunk = *chunk as usize;
        let Some(bytecode) = payload.bytecode.get(chunk) else {
            return Err(LoadError::CorruptArena);
        };
        if upvalues.len() != bytecode.upvalues.len() {
            return Err(invalid(
                chunk,
                None,
                "closure upvalue count does not match bytecode",
            ));
        }
        if upvalues
            .iter()
            .any(|id| (*id as usize) >= payload.upvalues.len())
        {
            return Err(LoadError::CorruptArena);
        }
    }
    Ok(())
}

pub(super) fn verify_payload(payload: SavePayload) -> Result<VerifiedSavePayload, LoadError> {
    for (chunk, bytecode) in payload.bytecode.iter().enumerate() {
        verify::validate_bytecode(bytecode)
            .map_err(|error| invalid(chunk, error.instruction, error.reason))?;
    }
    // This precedes materialization, including build_bytecode's out[idx] access.
    validate_nested_graph(&payload.bytecode)?;
    validate_closures(&payload)?;
    Ok(VerifiedSavePayload(payload))
}
