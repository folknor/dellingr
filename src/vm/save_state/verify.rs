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

fn validate_value(value: &super::SavedVal, payload: &SavePayload) -> Result<(), LoadError> {
    match value {
        super::SavedVal::Str(id) if (*id as usize) >= payload.strings.len() => {
            Err(LoadError::CorruptArena)
        }
        super::SavedVal::Obj(id) if (*id as usize) >= payload.objects.len() => {
            Err(LoadError::CorruptArena)
        }
        _ => Ok(()),
    }
}

fn validate_environment_deltas(payload: &SavePayload) -> Result<(), LoadError> {
    let mut tokens = std::collections::BTreeSet::new();
    for delta in &payload.env_deltas {
        if !tokens.insert(&delta.token) {
            return Err(LoadError::CorruptArena);
        }
        let mut deleted = std::collections::BTreeSet::new();
        for key in &delta.deleted {
            validate_value(key, payload)?;
            if !deleted.insert(resolve_key(key, payload)?) {
                return Err(LoadError::CorruptArena);
            }
        }
        let mut upserts = std::collections::BTreeSet::new();
        for (key, value) in &delta.upserts {
            validate_value(key, payload)?;
            validate_value(value, payload)?;
            if matches!(value, super::SavedVal::Nil) || !upserts.insert(resolve_key(key, payload)?)
            {
                return Err(LoadError::CorruptArena);
            }
        }
        if !deleted.is_disjoint(&upserts) {
            return Err(LoadError::CorruptArena);
        }
        if let Some(order) = &delta.order {
            let mut order_keys = std::collections::BTreeSet::new();
            for key in order {
                validate_value(key, payload)?;
                let key = resolve_key(key, payload)?;
                if deleted.contains(&key) || !order_keys.insert(key) {
                    return Err(LoadError::CorruptArena);
                }
            }
            if !upserts.is_subset(&order_keys) {
                return Err(LoadError::CorruptArena);
            }
        }
        if let super::SavedMetatableDelta::Set(value) = &delta.metatable {
            if !matches!(value, super::SavedVal::Obj(_) | super::SavedVal::EnvObj(_)) {
                return Err(LoadError::CorruptArena);
            }
            validate_value(value, payload)?;
        }
    }
    Ok(())
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
enum ResolvedKey {
    Bool(bool),
    Num(u64),
    Str(Vec<u8>),
    Obj(u32),
    Fn(String),
    EnvObj(String),
}

fn resolve_key(value: &super::SavedVal, payload: &SavePayload) -> Result<ResolvedKey, LoadError> {
    match value {
        super::SavedVal::Nil => Err(LoadError::CorruptArena),
        super::SavedVal::Bool(value) => Ok(ResolvedKey::Bool(*value)),
        super::SavedVal::Num(bits) => {
            let number = f64::from_bits(*bits);
            if number.is_nan() {
                Err(LoadError::CorruptArena)
            } else {
                Ok(ResolvedKey::Num(if number == 0.0 { 0 } else { *bits }))
            }
        }
        super::SavedVal::Str(id) => payload
            .strings
            .get(*id as usize)
            .cloned()
            .map(ResolvedKey::Str)
            .ok_or(LoadError::CorruptArena),
        super::SavedVal::Obj(id) => Ok(ResolvedKey::Obj(*id)),
        super::SavedVal::Fn(id) => Ok(ResolvedKey::Fn(id.clone())),
        super::SavedVal::EnvObj(id) => Ok(ResolvedKey::EnvObj(id.clone())),
    }
}

fn validate_references(payload: &SavePayload) -> Result<(), LoadError> {
    for value in &payload.upvalues {
        validate_value(value, payload)?;
    }
    for object in &payload.objects {
        if let SavedObject::Table { entries, metatable } = object {
            for (key, value) in entries {
                validate_value(key, payload)?;
                validate_value(value, payload)?;
            }
            if let Some(value) = metatable {
                validate_value(value, payload)?;
            }
        }
    }
    for (_, value) in &payload.user_globals {
        validate_value(value, payload)?;
    }
    Ok(())
}

fn validate_pointer_ids(payload: &SavePayload) -> Result<(), LoadError> {
    let mut ids = std::collections::BTreeSet::new();
    // Duplicate detection is split by variant and kept in ordered sets rather
    // than a linear scan: the list length is attacker-controlled, so an O(n^2)
    // check would make a forged save expensive to reject.
    let mut values = std::collections::BTreeSet::new();
    for (value, id) in &payload.format_pointer_ids {
        let first_occurrence = match value {
            super::SavedVal::Obj(_)
            | super::SavedVal::EnvObj(_)
            | super::SavedVal::Str(_)
            | super::SavedVal::Fn(_) => values.insert(resolve_key(value, payload)?),
            _ => return Err(LoadError::CorruptArena),
        };
        if !first_occurrence || !ids.insert(*id) {
            return Err(LoadError::CorruptArena);
        }
        validate_value(value, payload)?;
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
    validate_references(&payload)?;
    validate_environment_deltas(&payload)?;
    validate_pointer_ids(&payload)?;
    Ok(VerifiedSavePayload(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(delta: super::super::SavedEnvDelta) -> SavePayload {
        SavePayload {
            has_standard_environment: true,
            rng_state: 0,
            cost_remaining: 0,
            cost_budget: 0,
            cost_budget_configured: false,
            cost_used: 0,
            strings: vec![b"same".to_vec(), b"same".to_vec()],
            bytecode: Vec::new(),
            upvalues: Vec::new(),
            objects: Vec::new(),
            user_globals: Vec::new(),
            env_deltas: vec![delta],
            next_format_pointer_id: 1,
            format_pointer_ids: Vec::new(),
        }
    }

    #[test]
    fn environment_delta_keys_use_runtime_equality_and_reject_invalid_forms() {
        let unchanged = super::super::SavedMetatableDelta::Unchanged;
        let duplicate_strings = super::super::SavedEnvDelta {
            token: "math".to_string(),
            deleted: vec![super::super::SavedVal::Str(0)],
            upserts: vec![(
                super::super::SavedVal::Str(1),
                super::super::SavedVal::Bool(true),
            )],
            order: None,
            metatable: unchanged.clone(),
        };
        assert!(matches!(
            verify_payload(payload(duplicate_strings)),
            Err(LoadError::CorruptArena)
        ));

        let negative_zero = super::super::SavedEnvDelta {
            token: "math".to_string(),
            deleted: vec![super::super::SavedVal::Num(0)],
            upserts: vec![(
                super::super::SavedVal::Num((-0.0f64).to_bits()),
                super::super::SavedVal::Bool(true),
            )],
            order: None,
            metatable: unchanged.clone(),
        };
        assert!(matches!(
            verify_payload(payload(negative_zero)),
            Err(LoadError::CorruptArena)
        ));

        let invalid_order = super::super::SavedEnvDelta {
            token: "math".to_string(),
            deleted: Vec::new(),
            upserts: vec![(
                super::super::SavedVal::Bool(true),
                super::super::SavedVal::Nil,
            )],
            order: Some(Vec::new()),
            metatable: unchanged,
        };
        assert!(matches!(
            verify_payload(payload(invalid_order)),
            Err(LoadError::CorruptArena)
        ));
    }
}
