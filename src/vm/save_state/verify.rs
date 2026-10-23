use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::compiler::verify::{self, BytecodeView};

use super::super::table::{MAX_ARRAY_LEN, array_index, integer_index, is_append_key};
use super::{
    CorruptionReason, DuplicateEntry, EnvironmentCorruption, GmatchCorruption,
    InvalidBytecodeReason, KeyForm, SaveLocation, SavePayload, SavedBytecode, SavedObject,
    SavedReference, SnapshotError, TableCorruption, corrupt_arena as corrupt,
};

use crate::gmatch_schema;

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
    fn raw_instruction(&self, pc: usize) -> u64 {
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
    fn upvalue(&self, index: usize) -> crate::compiler::UpvalueDesc {
        self.upvalues[index]
    }
    fn is_vararg(&self) -> bool {
        self.is_vararg
    }
    fn line_info_len(&self) -> usize {
        self.line_info.len()
    }
}

// Preserve structured chunk/instruction fields here. SnapshotError owns their
// rendering, while the shared verifier owns the reason and compiler display.
fn invalid(chunk: usize, instruction: Option<u32>, reason: InvalidBytecodeReason) -> SnapshotError {
    SnapshotError::InvalidBytecode {
        chunk: u32::try_from(chunk)
            .expect("saved chunk indices are bounded by a u32 length prefix"),
        instruction,
        reason,
    }
}

fn rejected(chunk: usize, error: &verify::BytecodeValidationError) -> SnapshotError {
    invalid(
        chunk,
        error.instruction,
        InvalidBytecodeReason::Validation(error.reason),
    )
}

fn insert_unique<T: Ord>(
    set: &mut BTreeSet<T>,
    key: T,
    reason: CorruptionReason,
    index: usize,
    location: SaveLocation,
) -> Result<(), SnapshotError> {
    if set.insert(key) {
        Ok(())
    } else {
        Err(corrupt(reason, index, location))
    }
}

fn validate_nested_graph(bytecode: &[SavedBytecode]) -> Result<(), SnapshotError> {
    let mut indegree = vec![0usize; bytecode.len()];
    for (parent, chunk) in bytecode.iter().enumerate() {
        for child in &chunk.nested {
            let child = *child as usize;
            if child >= bytecode.len() {
                return Err(corrupt(
                    CorruptionReason::IdOutOfRange(SavedReference::NestedChunk),
                    child,
                    SaveLocation::Bytecode,
                ));
            }
            indegree[child] = indegree[child]
                .checked_add(1)
                .ok_or_else(|| invalid(parent, None, InvalidBytecodeReason::NestedGraphTooLarge))?;
            verify::validate_child(chunk, &bytecode[child])
                .map_err(|error| rejected(parent, &error))?;
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
            verify::validate_depth(depths[child]).map_err(|error| rejected(child, &error))?;
            indegree[child] -= 1;
            if indegree[child] == 0 {
                ready.push_back(child);
            }
        }
    }
    if visited != bytecode.len() {
        return Err(invalid(0, None, InvalidBytecodeReason::NestedGraphCycle));
    }
    Ok(())
}

fn validate_closures(payload: &SavePayload) -> Result<(), SnapshotError> {
    for object in &payload.objects {
        let SavedObject::Closure { chunk, upvalues } = object else {
            continue;
        };
        let chunk = *chunk as usize;
        let Some(bytecode) = payload.bytecode.get(chunk) else {
            return Err(corrupt(
                CorruptionReason::IdOutOfRange(SavedReference::ClosureChunk),
                chunk,
                SaveLocation::Bytecode,
            ));
        };
        if upvalues.len() != bytecode.upvalues.len() {
            return Err(invalid(
                chunk,
                None,
                InvalidBytecodeReason::ClosureUpvalueCountMismatch,
            ));
        }
        for id in upvalues {
            if (*id as usize) >= payload.upvalues.len() {
                return Err(corrupt(
                    CorruptionReason::IdOutOfRange(SavedReference::ClosureUpvalue),
                    *id as usize,
                    SaveLocation::Upvalues,
                ));
            }
        }
    }
    Ok(())
}

fn validate_value(value: &super::SavedVal, payload: &SavePayload) -> Result<(), SnapshotError> {
    match value {
        super::SavedVal::Str(id) if (*id as usize) >= payload.strings.len() => Err(corrupt(
            CorruptionReason::IdOutOfRange(SavedReference::String),
            *id as usize,
            SaveLocation::Strings,
        )),
        super::SavedVal::Obj(id) if (*id as usize) >= payload.objects.len() => Err(corrupt(
            CorruptionReason::IdOutOfRange(SavedReference::Object),
            *id as usize,
            SaveLocation::Objects,
        )),
        _ => Ok(()),
    }
}

fn validate_environment_deltas(payload: &SavePayload) -> Result<(), SnapshotError> {
    let mut tokens = BTreeSet::new();
    for (delta_index, delta) in payload.env_deltas.iter().enumerate() {
        insert_unique(
            &mut tokens,
            &delta.token,
            CorruptionReason::Duplicate {
                entry: DuplicateEntry::EnvironmentToken,
                form: KeyForm::Saved,
            },
            delta_index,
            SaveLocation::EnvironmentDeltas,
        )?;
        // This pass checks the saved representation (`Fn` ids, environment
        // tokens). It cannot be the only check: two distinct ids can resolve
        // to one function address (identical-code folding) and two tokens to
        // one object, so `materialize_payload` repeats the checks on runtime
        // values and its failures are the ones that catch those cases.
        let mut deleted = BTreeSet::new();
        for (key_index, key) in delta.deleted.iter().enumerate() {
            validate_value(key, payload)?;
            let resolved = key.table_key(
                &payload.strings,
                key_index,
                SaveLocation::EnvironmentDeleted(delta_index),
            )?;
            insert_unique(
                &mut deleted,
                resolved,
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::DeletedEnvironmentKey,
                    form: KeyForm::Saved,
                },
                key_index,
                SaveLocation::EnvironmentDeleted(delta_index),
            )?;
        }
        let mut upserts = BTreeSet::new();
        for (entry_index, (key, value)) in delta.upserts.iter().enumerate() {
            validate_value(key, payload)?;
            validate_value(value, payload)?;
            if matches!(value, super::SavedVal::Nil) {
                return Err(corrupt(
                    CorruptionReason::Environment(EnvironmentCorruption::UpsertStoresNil),
                    entry_index,
                    SaveLocation::EnvironmentUpserts(delta_index),
                ));
            }
            let resolved = key.table_key(
                &payload.strings,
                entry_index,
                SaveLocation::EnvironmentUpserts(delta_index),
            )?;
            insert_unique(
                &mut upserts,
                resolved,
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::EnvironmentUpsertKey,
                    form: KeyForm::Saved,
                },
                entry_index,
                SaveLocation::EnvironmentUpserts(delta_index),
            )?;
        }
        if !deleted.is_disjoint(&upserts) {
            return Err(corrupt(
                CorruptionReason::Environment(EnvironmentCorruption::KeyDeletedAndUpserted(
                    KeyForm::Saved,
                )),
                delta_index,
                SaveLocation::EnvironmentDeltas,
            ));
        }
        if let Some(order) = &delta.order {
            let mut order_keys = BTreeSet::new();
            for (key_index, key) in order.iter().enumerate() {
                validate_value(key, payload)?;
                let key = key.table_key(
                    &payload.strings,
                    key_index,
                    SaveLocation::EnvironmentOrder(delta_index),
                )?;
                if deleted.contains(&key) {
                    return Err(corrupt(
                        CorruptionReason::Environment(
                            EnvironmentCorruption::OrderContainsDeletedKey(KeyForm::Saved),
                        ),
                        key_index,
                        SaveLocation::EnvironmentOrder(delta_index),
                    ));
                }
                insert_unique(
                    &mut order_keys,
                    key,
                    CorruptionReason::Duplicate {
                        entry: DuplicateEntry::EnvironmentOrderKey,
                        form: KeyForm::Saved,
                    },
                    key_index,
                    SaveLocation::EnvironmentOrder(delta_index),
                )?;
            }
            if !upserts.is_subset(&order_keys) {
                return Err(corrupt(
                    CorruptionReason::Environment(EnvironmentCorruption::OrderOmitsUpsertedKey(
                        KeyForm::Saved,
                    )),
                    delta_index,
                    SaveLocation::EnvironmentDeltas,
                ));
            }
        }
        if let Some(slots) = &delta.hash_slots {
            let shape = super::SavedObject::Table {
                array_len: delta.array.as_ref().map_or(0, |array| array.len() as u64),
                array_slots: 0,
                entries: slots.clone(),
                metatable: None,
            };
            validate_table_shape(payload, &shape, delta_index)?;
        }
        for value in delta.array.iter().flatten() {
            validate_value(value, payload)?;
        }
        // The array describes shape, not a second independent set of writes.
        // Require its live slots to agree with the upserts. Canonical library
        // baselines contain only named fields, so every live numeric array
        // slot is an upsert, including one added by host setup before saving.
        let array = delta.array.as_deref().unwrap_or_default();
        let mut array_upserts = BTreeMap::new();
        for (entry_index, (key, value)) in delta.upserts.iter().enumerate() {
            if let super::SavedVal::Num(bits) = key {
                if is_append_key(f64::from_bits(*bits), array.len()) {
                    return Err(corrupt(
                        CorruptionReason::Environment(
                            EnvironmentCorruption::HashEntryBelongsInArray,
                        ),
                        entry_index,
                        SaveLocation::EnvironmentUpserts(delta_index),
                    ));
                }
                let n = f64::from_bits(*bits);
                if let Some(slot) = array_index(n, array.len()) {
                    array_upserts.insert(slot, value);
                }
            }
        }
        for (slot, value) in array.iter().enumerate() {
            let upsert = array_upserts.get(&slot).copied();
            if (matches!(value, super::SavedVal::Nil) && upsert.is_some())
                || (!matches!(value, super::SavedVal::Nil) && upsert != Some(value))
            {
                return Err(corrupt(
                    CorruptionReason::Environment(EnvironmentCorruption::ArrayDisagreesWithUpserts),
                    slot,
                    SaveLocation::EnvironmentArray(delta_index),
                ));
            }
        }
        if let Some(order) = &delta.order {
            let mut ordered = order.iter();
            for (slot, value) in array.iter().enumerate() {
                if !matches!(value, super::SavedVal::Nil)
                    && ordered.next() != Some(&super::SavedVal::Num(((slot + 1) as f64).to_bits()))
                {
                    return Err(corrupt(
                        CorruptionReason::Environment(
                            EnvironmentCorruption::OrderDisagreesWithArray,
                        ),
                        slot,
                        SaveLocation::EnvironmentArray(delta_index),
                    ));
                }
            }
            for key in ordered {
                if let super::SavedVal::Num(bits) = key {
                    let n = f64::from_bits(*bits);
                    if array_index(n, array.len()).is_some() || is_append_key(n, array.len()) {
                        return Err(corrupt(
                            CorruptionReason::Environment(
                                EnvironmentCorruption::HashOrderBelongsInArray,
                            ),
                            delta_index,
                            SaveLocation::EnvironmentDeltas,
                        ));
                    }
                }
            }
        }
        if let super::SavedMetatableDelta::Set(value) = &delta.metatable {
            if !matches!(value, super::SavedVal::Obj(_) | super::SavedVal::EnvObj(_)) {
                return Err(corrupt(
                    CorruptionReason::Environment(EnvironmentCorruption::MetatableNotObject),
                    delta_index,
                    SaveLocation::EnvironmentDeltas,
                ));
            }
            validate_value(value, payload)?;
            validate_metatable(
                value,
                payload,
                CorruptionReason::Environment(EnvironmentCorruption::MetatableNotTable(
                    KeyForm::Saved,
                )),
                delta_index,
                SaveLocation::EnvironmentMetatable(delta_index),
            )?;
        }
    }
    Ok(())
}

/// A metatable is always a table at runtime (`setmetatable` refuses anything
/// else, and `getmetatable` plus every metamethod lookup assume it). A saved
/// `Obj` metatable must therefore name a saved table, not a closure.
/// `EnvObj` tokens are resolved, and their runtime type checked, at
/// materialization, since the environment is not part of the payload.
fn validate_metatable(
    value: &super::SavedVal,
    payload: &SavePayload,
    reason: CorruptionReason,
    owner_index: usize,
    location: SaveLocation,
) -> Result<(), SnapshotError> {
    match value {
        super::SavedVal::Obj(id) => match payload.objects.get(*id as usize) {
            Some(SavedObject::Table { .. }) => Ok(()),
            _ => Err(corrupt(reason, *id as usize, SaveLocation::Objects)),
        },
        super::SavedVal::EnvObj(_) => Ok(()),
        _ => Err(corrupt(reason, owner_index, location)),
    }
}

fn validate_table_shape(
    payload: &SavePayload,
    object: &SavedObject,
    object_index: usize,
) -> Result<(), SnapshotError> {
    if let SavedObject::Table {
        array_len,
        array_slots,
        entries,
        metatable,
    } = object
    {
        let location = SaveLocation::TableEntries {
            object: object_index,
        };
        let array_slots = *array_slots as usize;
        if array_slots > entries.len() {
            return Err(corrupt(
                CorruptionReason::Table(TableCorruption::ArraySlotsExceedEntries),
                array_slots,
                location,
            ));
        }
        // The logical length is not backed by stored slots, so bound it
        // explicitly: past MAX_ARRAY_LEN array keys stop being exact,
        // distinct numbers and tagged slot indices could collide.
        let array_len = match usize::try_from(*array_len) {
            Ok(len) if len <= MAX_ARRAY_LEN => Some(len),
            _ => None,
        };
        let Some(array_len) = array_len else {
            return Err(corrupt(
                CorruptionReason::Table(TableCorruption::ArrayLengthExceedsLimit),
                object_index,
                location,
            ));
        };
        // Do not bound logical length by the live span plus fixed slack:
        // constructors append nil structurally, so valid live tables can
        // have arbitrarily long nil tails (or no live span at all).
        // Rejecting those would break save/load for valid states. This
        // representation bound alone does not bound later gap allocations;
        // handling allocation failure belongs to the table write path.
        // The stored span is canonical: consecutive keys inside the
        // logical array, starting and ending at a live slot.
        let first_slot = match entries.first() {
            Some((super::SavedVal::Num(bits), _)) if array_slots > 0 => {
                array_index(f64::from_bits(*bits), array_len)
                    .filter(|slot| slot + array_slots <= array_len)
            }
            _ => Some(0),
        };
        let Some(first_slot) = first_slot else {
            return Err(corrupt(
                CorruptionReason::Table(TableCorruption::ArraySlotWrongKey),
                0,
                location,
            ));
        };
        if array_slots > 0
            && (matches!(entries[0].1, super::SavedVal::Nil)
                || matches!(entries[array_slots - 1].1, super::SavedVal::Nil))
        {
            return Err(corrupt(
                CorruptionReason::Table(TableCorruption::ArraySpanEndpointsNotLive),
                0,
                location,
            ));
        }
        let mut keys = BTreeSet::new();
        for (entry_index, (key, value)) in entries.iter().enumerate() {
            validate_value(key, payload)?;
            validate_value(value, payload)?;
            if entry_index < array_slots {
                // Array slots carry their implied key, and may be empty.
                if !matches!(key, super::SavedVal::Num(bits) if array_index(f64::from_bits(*bits), array_len) == Some(first_slot + entry_index))
                {
                    return Err(corrupt(
                        CorruptionReason::Table(TableCorruption::ArraySlotWrongKey),
                        entry_index,
                        SaveLocation::TableEntries {
                            object: object_index,
                        },
                    ));
                }
            } else {
                // Only dead controls may overlap the logical array; live
                // values for these keys belong in the array span.
                if !matches!(value, super::SavedVal::Nil)
                    && matches!(key, super::SavedVal::Num(bits) if array_index(f64::from_bits(*bits), array_len).is_some())
                {
                    return Err(corrupt(
                        CorruptionReason::Table(TableCorruption::HashEntryBelongsInArray),
                        entry_index,
                        SaveLocation::TableEntries {
                            object: object_index,
                        },
                    ));
                }
                // A live hash entry at the array's append key would have
                // migrated into the array, so no engine save holds one.
                if !matches!(value, super::SavedVal::Nil)
                    && matches!(key, super::SavedVal::Num(bits) if is_append_key(f64::from_bits(*bits), array_len))
                {
                    return Err(corrupt(
                        CorruptionReason::Table(TableCorruption::HashEntryBelongsInArray),
                        entry_index,
                        SaveLocation::TableEntries {
                            object: object_index,
                        },
                    ));
                }
            }
            if entry_index == array_slots
                && matches!(
                    (key, value),
                    (super::SavedVal::Nil, super::SavedVal::Bool(true))
                )
            {
                continue;
            }
            if entry_index >= array_slots
                && matches!((key, value), (super::SavedVal::Nil, super::SavedVal::Nil))
            {
                if !matches!(
                    entries.get(array_slots),
                    Some((super::SavedVal::Nil, super::SavedVal::Bool(true)))
                ) {
                    return Err(corrupt(
                        CorruptionReason::Table(TableCorruption::EntryStoresNil),
                        entry_index,
                        SaveLocation::TableEntries {
                            object: object_index,
                        },
                    ));
                }
                continue;
            }
            // Array keys are implied and may also have an older dead hash
            // control slot. Check uniqueness only within the hash part.
            if entry_index < array_slots {
                continue;
            }
            let key = key.table_key(
                &payload.strings,
                entry_index,
                SaveLocation::TableEntries {
                    object: object_index,
                },
            )?;
            insert_unique(
                &mut keys,
                key,
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::TableKey,
                    form: KeyForm::Saved,
                },
                entry_index,
                SaveLocation::TableEntries {
                    object: object_index,
                },
            )?;
        }
        if let Some(value) = metatable {
            validate_value(value, payload)?;
            validate_metatable(
                value,
                payload,
                CorruptionReason::Table(TableCorruption::MetatableNotTable),
                object_index,
                SaveLocation::Objects,
            )?;
        }
    }
    Ok(())
}

fn validate_references(payload: &SavePayload) -> Result<(), SnapshotError> {
    for value in &payload.upvalues {
        validate_value(value, payload)?;
    }
    for (object_index, object) in payload.objects.iter().enumerate() {
        validate_table_shape(payload, object, object_index)?;
    }
    let mut global_names = BTreeSet::new();
    for (index, (name, value)) in payload.user_globals.iter().enumerate() {
        insert_unique(
            &mut global_names,
            name.as_str(),
            CorruptionReason::Duplicate {
                entry: DuplicateEntry::GlobalName,
                form: KeyForm::Saved,
            },
            index,
            SaveLocation::Globals,
        )?;
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

fn table_numeric_field(
    payload: &SavePayload,
    table_id: u32,
    field: f64,
) -> Option<&super::SavedVal> {
    let Some(super::SavedObject::Table { entries, .. }) = payload.objects.get(table_id as usize)
    else {
        return None;
    };
    let field_bits = field.to_bits();
    entries.iter().find_map(|(key, value)| {
        let super::SavedVal::Num(bits) = key else {
            return None;
        };
        (*bits == field_bits).then_some(value)
    })
}

fn gmatch_integer_field(
    payload: &SavePayload,
    table_id: u32,
    field: &[u8],
    maximum: usize,
    reason: CorruptionReason,
) -> Result<usize, SnapshotError> {
    let Some(super::SavedVal::Num(bits)) = table_field(payload, table_id, field) else {
        return Err(corrupt(reason, table_id as usize, SaveLocation::Objects));
    };
    let value = f64::from_bits(*bits);
    integer_index(value, maximum)
        .ok_or_else(|| corrupt(reason, table_id as usize, SaveLocation::Objects))
}

fn validate_gmatch_last_match(
    payload: &SavePayload,
    table_id: u32,
    subject_len: usize,
) -> Result<(), SnapshotError> {
    if table_field(
        payload,
        table_id,
        gmatch_schema::LAST_MATCH_FIELD.as_bytes(),
    )
    .is_some()
    {
        gmatch_integer_field(
            payload,
            table_id,
            gmatch_schema::LAST_MATCH_FIELD.as_bytes(),
            subject_len,
            CorruptionReason::Gmatch(GmatchCorruption::LastMatchInvalid),
        )?;
    }
    Ok(())
}

fn validate_gmatch_compile_state(
    payload: &SavePayload,
    table_id: u32,
) -> Result<(), SnapshotError> {
    let Some(super::SavedVal::Str(cache_key_id)) =
        table_numeric_field(payload, table_id, gmatch_schema::COMPILE_CHARGED_SLOT)
    else {
        return Err(corrupt(
            CorruptionReason::Gmatch(GmatchCorruption::CompileFlagKeyMissing),
            table_id as usize,
            SaveLocation::Objects,
        ));
    };
    let Some(cache_key) = payload.strings.get(*cache_key_id as usize) else {
        return Err(corrupt(
            CorruptionReason::Gmatch(GmatchCorruption::CompileFlagKeyOutOfRange),
            table_id as usize,
            SaveLocation::Objects,
        ));
    };
    if cache_key.as_slice() != gmatch_schema::COMPILE_CHARGED_FIELD.as_bytes() {
        return Err(corrupt(
            CorruptionReason::Gmatch(GmatchCorruption::CompileFlagKeyInvalid),
            table_id as usize,
            SaveLocation::Objects,
        ));
    }
    if !matches!(
        table_field(
            payload,
            table_id,
            gmatch_schema::COMPILE_CHARGED_FIELD.as_bytes(),
        ),
        None | Some(super::SavedVal::Bool(true))
    ) {
        return Err(corrupt(
            CorruptionReason::Gmatch(GmatchCorruption::CompileFlagInvalid),
            table_id as usize,
            SaveLocation::Objects,
        ));
    }
    Ok(())
}

fn validate_gmatch_state(
    payload: &SavePayload,
    table_id: u32,
    empty_pattern: bool,
) -> Result<(), SnapshotError> {
    let maximum_position = crate::limits::MAX_STRING_BYTES + 1;
    if empty_pattern {
        let len = gmatch_integer_field(
            payload,
            table_id,
            gmatch_schema::LENGTH_FIELD.as_bytes(),
            crate::limits::MAX_STRING_BYTES,
            CorruptionReason::Gmatch(GmatchCorruption::EmptyLengthInvalid),
        )?;
        let pos = gmatch_integer_field(
            payload,
            table_id,
            gmatch_schema::POSITION_FIELD.as_bytes(),
            maximum_position,
            CorruptionReason::Gmatch(GmatchCorruption::EmptyPositionInvalid),
        )?;
        if pos > len.saturating_add(1) {
            return Err(corrupt(
                CorruptionReason::Gmatch(GmatchCorruption::EmptyPositionExceedsLength),
                table_id as usize,
                SaveLocation::Objects,
            ));
        }
        return Ok(());
    }

    let Some(super::SavedVal::Str(subject_id)) =
        table_field(payload, table_id, gmatch_schema::SUBJECT_FIELD.as_bytes())
    else {
        return Err(corrupt(
            CorruptionReason::Gmatch(GmatchCorruption::SubjectInvalid),
            table_id as usize,
            SaveLocation::Objects,
        ));
    };
    let Some(subject) = payload.strings.get(*subject_id as usize) else {
        return Err(corrupt(
            CorruptionReason::Gmatch(GmatchCorruption::SubjectStringOutOfRange),
            *subject_id as usize,
            SaveLocation::Strings,
        ));
    };
    if !matches!(
        table_field(payload, table_id, gmatch_schema::PATTERN_FIELD.as_bytes(),),
        Some(super::SavedVal::Str(_))
    ) {
        return Err(corrupt(
            CorruptionReason::Gmatch(GmatchCorruption::PatternInvalid),
            table_id as usize,
            SaveLocation::Objects,
        ));
    }
    let pos = gmatch_integer_field(
        payload,
        table_id,
        gmatch_schema::POSITION_FIELD.as_bytes(),
        maximum_position,
        CorruptionReason::Gmatch(GmatchCorruption::PositionInvalid),
    )?;
    if pos > subject.len().saturating_add(1) {
        return Err(corrupt(
            CorruptionReason::Gmatch(GmatchCorruption::PositionExceedsSubject),
            table_id as usize,
            SaveLocation::Objects,
        ));
    }
    validate_gmatch_last_match(payload, table_id, subject.len())?;
    validate_gmatch_compile_state(payload, table_id)?;
    Ok(())
}

fn validate_gmatch_iterators(payload: &SavePayload) -> Result<(), SnapshotError> {
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
            let value = payload.upvalues.get(*upvalue_id as usize).ok_or_else(|| {
                corrupt(
                    CorruptionReason::Gmatch(GmatchCorruption::ClosureUpvalueOutOfRange),
                    closure_index,
                    SaveLocation::Objects,
                )
            })?;
            match value {
                super::SavedVal::Fn(id) if id == gmatch_schema::ITERATOR_ID => {
                    if iterator_kind.replace(false).is_some() {
                        return Err(corrupt(
                            CorruptionReason::Gmatch(GmatchCorruption::MultipleIterators),
                            closure_index,
                            SaveLocation::Objects,
                        ));
                    }
                }
                super::SavedVal::Fn(id) if id == gmatch_schema::EMPTY_ITERATOR_ID => {
                    if iterator_kind.replace(true).is_some() {
                        return Err(corrupt(
                            CorruptionReason::Gmatch(GmatchCorruption::MultipleIterators),
                            closure_index,
                            SaveLocation::Objects,
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
                CorruptionReason::Gmatch(GmatchCorruption::MultipleStateObjects),
                closure_index,
                SaveLocation::Objects,
            ));
        }
        let Some(table_id) = state_table else {
            return Err(corrupt(
                CorruptionReason::Gmatch(GmatchCorruption::StateTableMissing),
                closure_index,
                SaveLocation::Objects,
            ));
        };
        if !matches!(
            payload.objects.get(table_id as usize),
            Some(super::SavedObject::Table { .. })
        ) {
            return Err(corrupt(
                CorruptionReason::Gmatch(GmatchCorruption::StateNotTable),
                table_id as usize,
                SaveLocation::Objects,
            ));
        }
        if validated_states.insert((table_id, empty_pattern)) {
            validate_gmatch_state(payload, table_id, empty_pattern)?;
        }
    }
    Ok(())
}

fn validate_pointer_ids(payload: &SavePayload) -> Result<(), SnapshotError> {
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
                let key = value.table_key(&payload.strings, index, SaveLocation::PointerIds)?;
                insert_unique(
                    &mut values,
                    key,
                    CorruptionReason::Duplicate {
                        entry: DuplicateEntry::FormatPointerValue,
                        form: KeyForm::Saved,
                    },
                    index,
                    SaveLocation::PointerIds,
                )?;
            }
            _ => {
                return Err(corrupt(
                    CorruptionReason::InvalidFormatPointerValue,
                    index,
                    SaveLocation::PointerIds,
                ));
            }
        }
        insert_unique(
            &mut ids,
            *id,
            CorruptionReason::Duplicate {
                entry: DuplicateEntry::FormatPointerId,
                form: KeyForm::Saved,
            },
            index,
            SaveLocation::PointerIds,
        )?;
        // The engine hands out ids from `next_format_pointer_id` upward, so a
        // saved id at or above the counter would let the next fresh value
        // render the same identity as a live one.
        if *id >= payload.next_format_pointer_id {
            return Err(corrupt(
                CorruptionReason::FormatPointerIdNotBelowNext,
                index,
                SaveLocation::PointerIds,
            ));
        }
        validate_value(value, payload)?;
    }
    Ok(())
}

pub(super) fn verify_payload(payload: SavePayload) -> Result<VerifiedSavePayload, SnapshotError> {
    for (chunk, bytecode) in payload.bytecode.iter().enumerate() {
        verify::validate_bytecode(bytecode).map_err(|error| rejected(chunk, &error))?;
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
            cost: crate::cost_meter::CostState::default(),
            strings: vec![b"same".to_vec(), b"same".to_vec()],
            bytecode: Vec::new(),
            upvalues: Vec::new(),
            objects: Vec::new(),
            user_globals: Vec::new(),
            globals_sealed: false,
            env_deltas: vec![delta],
            next_format_pointer_id: 1,
            format_pointer_ids: Vec::new(),
        }
    }

    fn gmatch_payload(empty_pattern: bool, position: super::super::SavedVal) -> SavePayload {
        let (iterator_id, strings, entries) = if empty_pattern {
            (
                gmatch_schema::EMPTY_ITERATOR_ID,
                vec![
                    gmatch_schema::POSITION_FIELD.as_bytes().to_vec(),
                    gmatch_schema::LENGTH_FIELD.as_bytes().to_vec(),
                    b"bad".to_vec(),
                ],
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
                gmatch_schema::ITERATOR_ID,
                vec![
                    gmatch_schema::SUBJECT_FIELD.as_bytes().to_vec(),
                    gmatch_schema::PATTERN_FIELD.as_bytes().to_vec(),
                    gmatch_schema::POSITION_FIELD.as_bytes().to_vec(),
                    b"abc".to_vec(),
                    b"a+".to_vec(),
                    b"bad".to_vec(),
                    gmatch_schema::COMPILE_CHARGED_FIELD.as_bytes().to_vec(),
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
                    (
                        super::super::SavedVal::Num(gmatch_schema::COMPILE_CHARGED_SLOT.to_bits()),
                        super::super::SavedVal::Str(6),
                    ),
                ],
            )
        };
        SavePayload {
            has_standard_environment: true,
            rng_state: 0,
            cost: crate::cost_meter::CostState::default(),
            strings,
            bytecode: Vec::new(),
            upvalues: vec![
                super::super::SavedVal::Fn(iterator_id.to_string()),
                super::super::SavedVal::Obj(0),
            ],
            objects: vec![
                super::super::SavedObject::Table {
                    array_len: 0,
                    array_slots: 0,
                    entries,
                    metatable: None,
                },
                super::super::SavedObject::Closure {
                    chunk: 0,
                    upvalues: vec![0, 1],
                },
            ],
            user_globals: Vec::new(),
            globals_sealed: false,
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
            array: None,
            hash_slots: None,
            metatable: unchanged.clone(),
        };
        assert!(matches!(
            verify_payload(payload(duplicate_strings)),
            Err(SnapshotError::CorruptArena { .. })
        ));

        let negative_zero = super::super::SavedEnvDelta {
            token: "math".to_string(),
            deleted: vec![super::super::SavedVal::Num(0)],
            upserts: vec![(
                super::super::SavedVal::Num((-0.0f64).to_bits()),
                super::super::SavedVal::Bool(true),
            )],
            order: None,
            array: None,
            hash_slots: None,
            metatable: unchanged.clone(),
        };
        assert!(matches!(
            verify_payload(payload(negative_zero)),
            Err(SnapshotError::CorruptArena { .. })
        ));

        let invalid_order = super::super::SavedEnvDelta {
            token: "math".to_string(),
            deleted: Vec::new(),
            upserts: vec![(
                super::super::SavedVal::Bool(true),
                super::super::SavedVal::Nil,
            )],
            order: Some(Vec::new()),
            array: None,
            hash_slots: None,
            metatable: unchanged,
        };
        assert!(matches!(
            verify_payload(payload(invalid_order)),
            Err(SnapshotError::CorruptArena { .. })
        ));
    }

    #[test]
    fn environment_array_must_agree_with_delta_membership_values_and_order() {
        let num = |n: f64| super::super::SavedVal::Num(n.to_bits());
        let live = super::super::SavedVal::Bool(true);
        let valid = super::super::SavedEnvDelta {
            token: "math".into(),
            deleted: Vec::new(),
            upserts: vec![(num(1.0), live.clone()), (num(3.0), live.clone())],
            order: Some(vec![num(1.0), num(3.0)]),
            array: Some(vec![
                live.clone(),
                super::super::SavedVal::Nil,
                live.clone(),
            ]),
            hash_slots: None,
            metatable: super::super::SavedMetatableDelta::Unchanged,
        };
        assert!(verify_payload(payload(valid.clone())).is_ok());

        let mut missing = valid.clone();
        missing.upserts.remove(0);
        let mut wrong_value = valid.clone();
        wrong_value.upserts[0].1 = super::super::SavedVal::Bool(false);
        let mut hole_upsert = valid.clone();
        hole_upsert.upserts.push((num(2.0), live.clone()));
        hole_upsert.order = None;
        let mut append_upsert = valid.clone();
        append_upsert.upserts.push((num(4.0), live.clone()));
        append_upsert.order = None;
        let mut wrong_order = valid.clone();
        wrong_order.order = Some(vec![num(3.0), num(1.0)]);
        let mut hole_order = valid.clone();
        hole_order.order = Some(vec![num(1.0), num(3.0), num(2.0)]);
        let mut no_array = valid;
        no_array.array = None;
        for delta in [
            missing,
            wrong_value,
            hole_upsert,
            append_upsert,
            wrong_order,
            hole_order,
            no_array,
        ] {
            assert!(matches!(
                verify_payload(payload(delta)),
                Err(SnapshotError::CorruptArena { .. })
            ));
        }
    }

    #[test]
    fn gmatch_snapshot_state_requires_valid_numeric_positions() {
        let valid_number = super::super::SavedVal::Num(1.0f64.to_bits());
        assert!(validate_gmatch_iterators(&gmatch_payload(false, valid_number.clone())).is_ok());
        assert!(validate_gmatch_iterators(&gmatch_payload(true, valid_number.clone())).is_ok());

        let malformed = super::super::SavedVal::Str(2);
        assert!(matches!(
            validate_gmatch_iterators(&gmatch_payload(false, malformed.clone())),
            Err(SnapshotError::CorruptArena {
                reason: CorruptionReason::Gmatch(GmatchCorruption::PositionInvalid),
                index: 0,
                location: SaveLocation::Objects,
            })
        ));
        assert!(matches!(
            validate_gmatch_iterators(&gmatch_payload(true, malformed)),
            Err(SnapshotError::CorruptArena {
                reason: CorruptionReason::Gmatch(GmatchCorruption::EmptyPositionInvalid),
                index: 0,
                location: SaveLocation::Objects,
            })
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
            Err(SnapshotError::CorruptArena {
                reason: CorruptionReason::Gmatch(GmatchCorruption::EmptyLengthInvalid),
                index: 0,
                location: SaveLocation::Objects,
            })
        ));
    }

    #[test]
    fn saved_table_array_prefix_is_checked() {
        let num = |n: f64| super::super::SavedVal::Num(n.to_bits());
        let with_table =
            |array_len: u64,
             array_slots: u32,
             entries: Vec<(super::super::SavedVal, super::super::SavedVal)>| {
                let mut payload = gmatch_payload(false, num(1.0));
                payload.objects = vec![super::super::SavedObject::Table {
                    array_len,
                    array_slots,
                    entries,
                    metatable: None,
                }];
                payload.upvalues = Vec::new();
                payload
            };
        let nil = super::super::SavedVal::Nil;
        let live = super::super::SavedVal::Bool(true);
        // Empty slots inside the stored span are legal, the span may start
        // above key 1 and end below the logical length, and a gap key in the
        // hash part is legal too.
        for (array_len, array_slots, entries) in [
            (
                4,
                3,
                vec![
                    (num(2.0), live.clone()),
                    (num(3.0), nil.clone()),
                    (num(4.0), live.clone()),
                    (num(6.0), live.clone()),
                ],
            ),
            (1 << 40, 1, vec![(num(7.0), live.clone())]),
            (9, 0, vec![(num(11.0), live.clone())]),
        ] {
            assert!(validate_references(&with_table(array_len, array_slots, entries)).is_ok());
        }
        for (array_len, array_slots, entries) in [
            // More slots than entries.
            (2, 2, vec![(num(1.0), live.clone())]),
            // Span reaching past the logical length.
            (
                2,
                2,
                vec![(num(2.0), live.clone()), (num(3.0), live.clone())],
            ),
            // Array slot with the wrong key.
            (
                3,
                2,
                vec![(num(1.0), live.clone()), (num(3.0), live.clone())],
            ),
            // A span must start and end at live slots.
            (
                2,
                2,
                vec![(num(1.0), nil.clone()), (num(2.0), live.clone())],
            ),
            (
                2,
                2,
                vec![(num(1.0), live.clone()), (num(2.0), nil.clone())],
            ),
            // Live hash entry at the append key or at an unstored array key.
            (
                1,
                1,
                vec![(num(1.0), live.clone()), (num(2.0), live.clone())],
            ),
            (0, 0, vec![(num(1.0), live.clone())]),
            (
                5,
                1,
                vec![(num(4.0), live.clone()), (num(2.0), live.clone())],
            ),
            // Array key repeated in the hash part.
            (
                1,
                1,
                vec![(num(1.0), live.clone()), (num(1.0), live.clone())],
            ),
            // Logical length beyond the representation limit.
            (u64::MAX, 0, vec![]),
            (MAX_ARRAY_LEN as u64 + 1, 0, vec![]),
        ] {
            assert!(matches!(
                validate_references(&with_table(array_len, array_slots, entries)),
                Err(SnapshotError::CorruptArena { .. })
            ));
        }
    }

    #[test]
    fn saved_metatables_must_be_tables() {
        // Object 0 is a table whose metatable is object `mt`; object 1 is a
        // table, object 2 a closure.
        let with_metatable = |mt: super::super::SavedVal| {
            let mut payload = gmatch_payload(false, super::super::SavedVal::Num(1.0f64.to_bits()));
            payload.objects = vec![
                super::super::SavedObject::Table {
                    array_len: 0,
                    array_slots: 0,
                    entries: Vec::new(),
                    metatable: Some(mt),
                },
                super::super::SavedObject::Table {
                    array_len: 0,
                    array_slots: 0,
                    entries: Vec::new(),
                    metatable: None,
                },
                super::super::SavedObject::Closure {
                    chunk: 0,
                    upvalues: Vec::new(),
                },
            ];
            payload.upvalues = Vec::new();
            payload
        };
        assert!(validate_references(&with_metatable(super::super::SavedVal::Obj(1))).is_ok());
        assert!(
            validate_references(&with_metatable(super::super::SavedVal::EnvObj(
                "math".to_string()
            )))
            .is_ok()
        );
        for bad in [
            super::super::SavedVal::Obj(2),
            super::super::SavedVal::Bool(true),
            super::super::SavedVal::Str(0),
        ] {
            assert!(matches!(
                validate_references(&with_metatable(bad)),
                Err(SnapshotError::CorruptArena { .. })
            ));
        }

        let mut env = payload(super::super::SavedEnvDelta {
            token: "math".to_string(),
            deleted: Vec::new(),
            upserts: Vec::new(),
            order: None,
            array: None,
            hash_slots: None,
            metatable: super::super::SavedMetatableDelta::Set(super::super::SavedVal::Obj(0)),
        });
        env.objects = vec![super::super::SavedObject::Closure {
            chunk: 0,
            upvalues: Vec::new(),
        }];
        assert!(matches!(
            validate_environment_deltas(&env),
            Err(SnapshotError::CorruptArena { .. })
        ));
    }

    #[test]
    fn pointer_ids_must_sit_below_the_next_id() {
        let with_next = |next: u64| SavePayload {
            has_standard_environment: true,
            rng_state: 0,
            cost: crate::cost_meter::CostState::default(),
            strings: vec![b"x".to_vec()],
            bytecode: Vec::new(),
            upvalues: Vec::new(),
            objects: Vec::new(),
            user_globals: Vec::new(),
            globals_sealed: false,
            env_deltas: Vec::new(),
            next_format_pointer_id: next,
            format_pointer_ids: vec![(super::super::SavedVal::Str(0), 1)],
        };
        assert!(validate_pointer_ids(&with_next(2)).is_ok());
        for next in [0, 1] {
            assert!(matches!(
                validate_pointer_ids(&with_next(next)),
                Err(SnapshotError::CorruptArena { .. })
            ));
        }
    }

    #[test]
    fn duplicate_global_names_are_rejected() {
        let with_globals = |globals: Vec<(String, super::super::SavedVal)>| SavePayload {
            has_standard_environment: true,
            rng_state: 0,
            cost: crate::cost_meter::CostState::default(),
            strings: Vec::new(),
            bytecode: Vec::new(),
            upvalues: Vec::new(),
            objects: Vec::new(),
            user_globals: globals,
            globals_sealed: false,
            env_deltas: Vec::new(),
            next_format_pointer_id: 0,
            format_pointer_ids: Vec::new(),
        };
        let one = (String::from("a"), super::super::SavedVal::Bool(true));
        let two = (String::from("a"), super::super::SavedVal::Bool(false));
        let other = (String::from("b"), super::super::SavedVal::Bool(false));
        assert!(validate_references(&with_globals(vec![one.clone(), other])).is_ok());
        assert!(matches!(
            validate_references(&with_globals(vec![one, two])),
            Err(SnapshotError::CorruptArena { .. })
        ));
    }
}
