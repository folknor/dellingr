use std::collections::{BTreeSet, VecDeque};

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

fn invalid(chunk: usize, instruction: Option<u32>, reason: impl Into<String>) -> LoadError {
    LoadError::InvalidBytecode {
        chunk: u32::try_from(chunk)
            .expect("saved chunk indices are bounded by a u32 length prefix"),
        instruction,
        reason: reason.into(),
    }
}

fn corrupt(reason: &'static str, index: usize) -> LoadError {
    LoadError::CorruptArena { reason, index }
}

fn insert_unique<T: Ord>(
    set: &mut BTreeSet<T>,
    key: T,
    reason: &'static str,
    index: usize,
) -> Result<(), LoadError> {
    if set.insert(key) {
        Ok(())
    } else {
        Err(corrupt(reason, index))
    }
}

fn validate_nested_graph(bytecode: &[SavedBytecode]) -> Result<(), LoadError> {
    let mut indegree = vec![0usize; bytecode.len()];
    for (parent, chunk) in bytecode.iter().enumerate() {
        for child in &chunk.nested {
            let child = *child as usize;
            if child >= bytecode.len() {
                return Err(corrupt("nested chunk id is out of range", child));
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
            return Err(corrupt("closure chunk id is out of range", chunk));
        };
        if upvalues.len() != bytecode.upvalues.len() {
            return Err(invalid(
                chunk,
                None,
                "closure upvalue count does not match bytecode",
            ));
        }
        for id in upvalues {
            if (*id as usize) >= payload.upvalues.len() {
                return Err(corrupt("closure upvalue id is out of range", *id as usize));
            }
        }
    }
    Ok(())
}

fn validate_value(value: &super::SavedVal, payload: &SavePayload) -> Result<(), LoadError> {
    match value {
        super::SavedVal::Str(id) if (*id as usize) >= payload.strings.len() => {
            Err(corrupt("string id is out of range", *id as usize))
        }
        super::SavedVal::Obj(id) if (*id as usize) >= payload.objects.len() => {
            Err(corrupt("object id is out of range", *id as usize))
        }
        _ => Ok(()),
    }
}

fn validate_environment_deltas(payload: &SavePayload) -> Result<(), LoadError> {
    let mut tokens = BTreeSet::new();
    for (delta_index, delta) in payload.env_deltas.iter().enumerate() {
        insert_unique(
            &mut tokens,
            &delta.token,
            "duplicate environment token",
            delta_index,
        )?;
        let mut deleted = BTreeSet::new();
        for (key_index, key) in delta.deleted.iter().enumerate() {
            validate_value(key, payload)?;
            let resolved = resolve_key(key, payload, key_index)?;
            insert_unique(
                &mut deleted,
                resolved,
                "duplicate deleted environment key",
                key_index,
            )?;
        }
        let mut upserts = BTreeSet::new();
        for (entry_index, (key, value)) in delta.upserts.iter().enumerate() {
            validate_value(key, payload)?;
            validate_value(value, payload)?;
            if matches!(value, super::SavedVal::Nil) {
                return Err(corrupt("environment upsert stores nil", entry_index));
            }
            let resolved = resolve_key(key, payload, entry_index)?;
            insert_unique(
                &mut upserts,
                resolved,
                "duplicate environment upsert key",
                entry_index,
            )?;
        }
        if !deleted.is_disjoint(&upserts) {
            return Err(corrupt(
                "environment key is both deleted and upserted",
                delta_index,
            ));
        }
        if let Some(order) = &delta.order {
            let mut order_keys = BTreeSet::new();
            for (key_index, key) in order.iter().enumerate() {
                validate_value(key, payload)?;
                let key = resolve_key(key, payload, key_index)?;
                if deleted.contains(&key) {
                    return Err(corrupt(
                        "environment order contains a deleted key",
                        key_index,
                    ));
                }
                insert_unique(
                    &mut order_keys,
                    key,
                    "duplicate environment order key",
                    key_index,
                )?;
            }
            if !upserts.is_subset(&order_keys) {
                return Err(corrupt(
                    "environment order omits an upserted key",
                    delta_index,
                ));
            }
        }
        if let super::SavedMetatableDelta::Set(value) = &delta.metatable {
            if !matches!(value, super::SavedVal::Obj(_) | super::SavedVal::EnvObj(_)) {
                return Err(corrupt(
                    "environment metatable is not an object",
                    delta_index,
                ));
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

fn resolve_key(
    value: &super::SavedVal,
    payload: &SavePayload,
    index: usize,
) -> Result<ResolvedKey, LoadError> {
    // Refusal: vm/table.rs currently exposes no canonical key helper. It only
    // rejects nil and NaN on insert, while Val equality/hash implement the
    // number rule separately. The table owner must export that rule before
    // this verifier can call it without duplicating semantics.
    match value {
        super::SavedVal::Nil => Err(corrupt("nil is not a table key", index)),
        super::SavedVal::Bool(value) => Ok(ResolvedKey::Bool(*value)),
        super::SavedVal::Num(bits) => {
            let number = f64::from_bits(*bits);
            if number.is_nan() {
                Err(corrupt("NaN is not a table key", index))
            } else {
                Ok(ResolvedKey::Num(if number == 0.0 { 0 } else { *bits }))
            }
        }
        super::SavedVal::Str(id) => payload
            .strings
            .get(*id as usize)
            .cloned()
            .map(ResolvedKey::Str)
            .ok_or_else(|| corrupt("table key string id is out of range", *id as usize)),
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
            let mut keys = BTreeSet::new();
            for (entry_index, (key, value)) in entries.iter().enumerate() {
                validate_value(key, payload)?;
                validate_value(value, payload)?;
                let key = resolve_key(key, payload, entry_index)?;
                insert_unique(&mut keys, key, "duplicate saved table key", entry_index)?;
                if matches!(value, super::SavedVal::Nil) {
                    return Err(corrupt("saved table entry stores nil", entry_index));
                }
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
    let mut ids = BTreeSet::new();
    // Duplicate detection is split by variant and kept in ordered sets rather
    // than a linear scan: the list length is attacker-controlled, so an O(n^2)
    // check would make a forged save expensive to reject.
    let mut values = BTreeSet::new();
    for (index, (value, id)) in payload.format_pointer_ids.iter().enumerate() {
        match value {
            super::SavedVal::Obj(_)
            | super::SavedVal::EnvObj(_)
            | super::SavedVal::Str(_)
            | super::SavedVal::Fn(_) => {
                let key = resolve_key(value, payload, index)?;
                insert_unique(&mut values, key, "duplicate format pointer value", index)?;
            }
            _ => return Err(corrupt("invalid format pointer value", index)),
        }
        insert_unique(&mut ids, *id, "duplicate format pointer id", index)?;
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
            Err(LoadError::CorruptArena { .. })
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
            Err(LoadError::CorruptArena { .. })
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
            Err(LoadError::CorruptArena { .. })
        ));
    }
}
