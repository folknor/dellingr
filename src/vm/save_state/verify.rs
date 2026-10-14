use std::collections::{BTreeSet, VecDeque};

use crate::compiler::verify::{self, BytecodeView};
use crate::limits::MAX_CHUNK_NESTING;

use super::super::table::{
    InvalidTableKey, NormalizedTableKey, TableKeyInput, normalize_table_key,
};
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
    let mut depths = vec![0usize; bytecode.len()];
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
            if depths[child] > MAX_CHUNK_NESTING {
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
enum SavedObjectKey {
    Object(u32),
    EnvironmentObject(String),
}

type ResolvedKey = NormalizedTableKey<Vec<u8>, SavedObjectKey, String>;

fn resolve_key(
    value: &super::SavedVal,
    payload: &SavePayload,
    index: usize,
) -> Result<ResolvedKey, LoadError> {
    let key = match value {
        super::SavedVal::Nil => TableKeyInput::Nil,
        super::SavedVal::Bool(value) => TableKeyInput::Bool(*value),
        super::SavedVal::Num(bits) => TableKeyInput::Number(f64::from_bits(*bits)),
        super::SavedVal::Str(id) => TableKeyInput::String(
            payload
                .strings
                .get(*id as usize)
                .cloned()
                .ok_or_else(|| corrupt("table key string id is out of range", *id as usize))?,
        ),
        super::SavedVal::Obj(id) => TableKeyInput::Object(SavedObjectKey::Object(*id)),
        super::SavedVal::Fn(id) => TableKeyInput::RustFn(id.clone()),
        super::SavedVal::EnvObj(id) => {
            TableKeyInput::Object(SavedObjectKey::EnvironmentObject(id.clone()))
        }
    };
    normalize_table_key(key).map_err(|error| match error {
        InvalidTableKey::Nil => corrupt("nil is not a table key", index),
        InvalidTableKey::Nan => corrupt("NaN is not a table key", index),
    })
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

fn table_field<'a>(
    payload: &'a SavePayload,
    table_id: u32,
    field: &[u8],
) -> Option<&'a super::SavedVal> {
    let Some(super::SavedObject::Table { entries, .. }) = payload.objects.get(table_id as usize)
    else {
        return None;
    };
    entries.iter().find_map(|(key, value)| {
        let super::SavedVal::Str(id) = key else {
            return None;
        };
        payload
            .strings
            .get(*id as usize)
            .filter(|bytes| bytes.as_slice() == field)
            .map(|_| value)
    })
}

fn gmatch_integer_field(
    payload: &SavePayload,
    table_id: u32,
    field: &[u8],
    maximum: usize,
    reason: &'static str,
    index: usize,
) -> Result<usize, LoadError> {
    let Some(super::SavedVal::Num(bits)) = table_field(payload, table_id, field) else {
        return Err(corrupt(reason, index));
    };
    let value = f64::from_bits(*bits);
    if !value.is_finite()
        || value < 0.0
        || value.fract().to_bits() << 1 != 0
        || value > maximum as f64
    {
        return Err(corrupt(reason, index));
    }
    Ok(value as usize)
}

fn validate_gmatch_state(
    payload: &SavePayload,
    table_id: u32,
    empty_pattern: bool,
    index: usize,
) -> Result<(), LoadError> {
    let maximum_position = super::super::MAX_STRING_BYTES + 1;
    if empty_pattern {
        let len = gmatch_integer_field(
            payload,
            table_id,
            b"len",
            super::super::MAX_STRING_BYTES,
            "empty gmatch length is missing or invalid",
            index,
        )?;
        let pos = gmatch_integer_field(
            payload,
            table_id,
            b"pos",
            maximum_position,
            "empty gmatch position is missing or invalid",
            index,
        )?;
        if pos > len.saturating_add(1) {
            return Err(corrupt("empty gmatch position exceeds its length", index));
        }
        return Ok(());
    }

    let Some(super::SavedVal::Str(subject_id)) = table_field(payload, table_id, b"s") else {
        return Err(corrupt("gmatch subject is missing or invalid", index));
    };
    let Some(subject) = payload.strings.get(*subject_id as usize) else {
        return Err(corrupt("gmatch subject string id is out of range", index));
    };
    if !matches!(
        table_field(payload, table_id, b"p"),
        Some(super::SavedVal::Str(_))
    ) {
        return Err(corrupt("gmatch pattern is missing or invalid", index));
    }
    let pos = gmatch_integer_field(
        payload,
        table_id,
        b"pos",
        maximum_position,
        "gmatch position is missing or invalid",
        index,
    )?;
    if pos > subject.len().saturating_add(1) {
        return Err(corrupt("gmatch position exceeds its subject", index));
    }
    Ok(())
}

fn validate_gmatch_iterators(payload: &SavePayload) -> Result<(), LoadError> {
    let mut validated_states = BTreeSet::new();
    for (closure_index, object) in payload.objects.iter().enumerate() {
        let super::SavedObject::Closure { upvalues, .. } = object else {
            continue;
        };
        // Ordinary closures may capture any number of objects, so object
        // multiplicity only matters once a gmatch iterator upvalue is found.
        let mut iterator_kind = None;
        let mut state_table = None;
        let mut multiple_objects = false;
        for upvalue_id in upvalues {
            let value = payload
                .upvalues
                .get(*upvalue_id as usize)
                .ok_or_else(|| corrupt("gmatch closure upvalue is out of range", closure_index))?;
            match value {
                super::SavedVal::Fn(id) if id == "string.gmatch.iter" => {
                    if iterator_kind.replace(false).is_some() {
                        return Err(corrupt(
                            "gmatch closure captures multiple iterators",
                            closure_index,
                        ));
                    }
                }
                super::SavedVal::Fn(id) if id == "string.gmatch.empty_iter" => {
                    if iterator_kind.replace(true).is_some() {
                        return Err(corrupt(
                            "gmatch closure captures multiple iterators",
                            closure_index,
                        ));
                    }
                }
                super::SavedVal::Obj(id) => {
                    if state_table.replace(*id).is_some() {
                        multiple_objects = true;
                    }
                }
                _ => {}
            }
        }
        let Some(empty_pattern) = iterator_kind else {
            continue;
        };
        if multiple_objects {
            return Err(corrupt(
                "gmatch closure captures multiple state objects",
                closure_index,
            ));
        }
        let Some(table_id) = state_table else {
            return Err(corrupt(
                "gmatch closure state table is missing",
                closure_index,
            ));
        };
        if !matches!(
            payload.objects.get(table_id as usize),
            Some(super::SavedObject::Table { .. })
        ) {
            return Err(corrupt(
                "gmatch closure state is not a table",
                closure_index,
            ));
        }
        if validated_states.insert((table_id, empty_pattern)) {
            validate_gmatch_state(payload, table_id, empty_pattern, closure_index)?;
        }
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
    validate_gmatch_iterators(&payload)?;
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

    fn gmatch_payload(empty_pattern: bool, position: super::super::SavedVal) -> SavePayload {
        let (iterator_id, strings, entries) = if empty_pattern {
            (
                "string.gmatch.empty_iter",
                vec![b"pos".to_vec(), b"len".to_vec(), b"bad".to_vec()],
                vec![
                    (super::super::SavedVal::Str(0), position),
                    (
                        super::super::SavedVal::Str(1),
                        super::super::SavedVal::Num(2.0f64.to_bits()),
                    ),
                ],
            )
        } else {
            (
                "string.gmatch.iter",
                vec![
                    b"s".to_vec(),
                    b"p".to_vec(),
                    b"pos".to_vec(),
                    b"abc".to_vec(),
                    b"a+".to_vec(),
                    b"bad".to_vec(),
                ],
                vec![
                    (
                        super::super::SavedVal::Str(0),
                        super::super::SavedVal::Str(3),
                    ),
                    (
                        super::super::SavedVal::Str(1),
                        super::super::SavedVal::Str(4),
                    ),
                    (super::super::SavedVal::Str(2), position),
                ],
            )
        };
        SavePayload {
            has_standard_environment: true,
            rng_state: 0,
            cost_remaining: 0,
            cost_budget: 0,
            cost_budget_configured: false,
            cost_used: 0,
            strings,
            bytecode: Vec::new(),
            upvalues: vec![
                super::super::SavedVal::Fn(iterator_id.to_string()),
                super::super::SavedVal::Obj(0),
            ],
            objects: vec![
                super::super::SavedObject::Table {
                    entries,
                    metatable: None,
                },
                super::super::SavedObject::Closure {
                    chunk: 0,
                    upvalues: vec![0, 1],
                },
            ],
            user_globals: Vec::new(),
            env_deltas: Vec::new(),
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

    #[test]
    fn gmatch_snapshot_state_requires_valid_numeric_positions() {
        let valid_number = super::super::SavedVal::Num(1.0f64.to_bits());
        assert!(validate_gmatch_iterators(&gmatch_payload(false, valid_number.clone())).is_ok());
        assert!(validate_gmatch_iterators(&gmatch_payload(true, valid_number.clone())).is_ok());

        let malformed = super::super::SavedVal::Str(2);
        assert!(matches!(
            validate_gmatch_iterators(&gmatch_payload(false, malformed.clone())),
            Err(LoadError::CorruptArena { .. })
        ));
        assert!(matches!(
            validate_gmatch_iterators(&gmatch_payload(true, malformed)),
            Err(LoadError::CorruptArena { .. })
        ));

        let mut invalid_length = gmatch_payload(true, valid_number);
        let Some(super::super::SavedObject::Table { entries, .. }) =
            invalid_length.objects.first_mut()
        else {
            panic!("fixture has a state table");
        };
        entries[1].1 = super::super::SavedVal::Str(2);
        assert!(matches!(
            validate_gmatch_iterators(&invalid_length),
            Err(LoadError::CorruptArena { .. })
        ));
    }
}
