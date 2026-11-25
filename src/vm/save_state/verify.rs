use std::collections::{BTreeSet, VecDeque};

use crate::compiler::verify::{self, BytecodeView};

use super::super::table::{MAX_ARRAY_LEN, array_index, is_append_key};
use super::{
    CorruptionReason, DuplicateEntry, InvalidBytecodeReason, KeyForm, SaveLocation, SavePayload,
    SavedBytecode, SavedObject, SavedReference, SnapshotError, TableCorruption,
    corrupt_arena as corrupt,
};

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
    fn table_key_slots(&self) -> u8 {
        self.table_key_slots
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

/// The chunk runtime list is a set of bytecode ids in strictly ascending
/// order, and names every closure's chunk: a live closure always runs on
/// its chunk's State-local bundle, so a save without it is not one this
/// crate wrote. Nested chunks may or may not be listed, depending on
/// whether they had been instantiated before the save.
fn validate_chunk_runtimes(payload: &SavePayload) -> Result<(), SnapshotError> {
    let mut previous: Option<u32> = None;
    for (index, id) in payload.chunk_runtimes.iter().enumerate() {
        if (*id as usize) >= payload.bytecode.len() {
            return Err(corrupt(
                CorruptionReason::IdOutOfRange(SavedReference::RuntimeChunk),
                *id as usize,
                SaveLocation::Bytecode,
            ));
        }
        if previous.is_some_and(|previous| previous >= *id) {
            return Err(corrupt(
                CorruptionReason::ChunkRuntimesNotAscending,
                index,
                SaveLocation::Bytecode,
            ));
        }
        previous = Some(*id);
    }
    for object in &payload.objects {
        if let SavedObject::Closure { chunk, .. } = object
            && payload.chunk_runtimes.binary_search(chunk).is_err()
        {
            return Err(corrupt(
                CorruptionReason::ClosureChunkWithoutRuntime,
                *chunk as usize,
                SaveLocation::Bytecode,
            ));
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
    location: &SaveLocation,
) -> Result<(), SnapshotError> {
    if let SavedObject::Table {
        array_len,
        array_slots,
        entries,
        metatable,
    } = object
    {
        let array_slots = *array_slots as usize;
        if array_slots > entries.len() {
            return Err(corrupt(
                CorruptionReason::Table(TableCorruption::ArraySlotsExceedEntries),
                array_slots,
                location.clone(),
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
                location.clone(),
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
                location.clone(),
            ));
        };
        if array_slots > 0
            && (matches!(entries[0].1, super::SavedVal::Nil)
                || matches!(entries[array_slots - 1].1, super::SavedVal::Nil))
        {
            return Err(corrupt(
                CorruptionReason::Table(TableCorruption::ArraySpanEndpointsNotLive),
                0,
                location.clone(),
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
                        location.clone(),
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
                        location.clone(),
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
                        location.clone(),
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
                        location.clone(),
                    ));
                }
                continue;
            }
            // Array keys are implied and may also have an older dead hash
            // control slot. Check uniqueness only within the hash part.
            if entry_index < array_slots {
                continue;
            }
            let key = key.table_key(&payload.strings, entry_index, location.clone())?;
            insert_unique(
                &mut keys,
                key,
                CorruptionReason::Duplicate {
                    entry: DuplicateEntry::TableKey,
                    form: KeyForm::Saved,
                },
                entry_index,
                location.clone(),
            )?;
        }
        if let Some(value) = metatable {
            validate_value(value, payload)?;
            validate_metatable(
                value,
                payload,
                CorruptionReason::Table(TableCorruption::MetatableNotTable),
                object_index,
                // The metatable belongs to the arena object, not its entries;
                // environment hash slots never carry one.
                SaveLocation::Objects,
            )?;
        }
    }
    Ok(())
}

/// A saved gmatch iterator holds only a state a live iterator can reach
/// (`GmatchIter`): the step trusts its record, so verification, before any
/// host setup runs, is where an edited save is refused. The pattern's own
/// syntax is not checked: a malformed pattern is a live iterator's state
/// too, and its error stays deferred to the search that reaches it.
pub(super) fn validate_gmatch(
    payload: &SavePayload,
    object: &SavedObject,
    object_index: usize,
) -> Result<(), SnapshotError> {
    let invalid = || {
        corrupt(
            CorruptionReason::InvalidGmatchIterator,
            object_index,
            SaveLocation::Objects,
        )
    };
    let string_len = |value: &super::SavedVal| match value {
        super::SavedVal::Str(id) => payload
            .strings
            .get(*id as usize)
            .map(Vec::len)
            .ok_or_else(invalid),
        _ => Err(invalid()),
    };
    let within = |value: u64, maximum: usize| match usize::try_from(value) {
        Ok(value) if value <= maximum => Ok(()),
        _ => Err(invalid()),
    };
    match object {
        SavedObject::GmatchIter {
            subject,
            pattern,
            pos,
            resumed,
        } => {
            let len = string_len(subject)?;
            if string_len(pattern)? == 0 {
                return Err(invalid());
            }
            // A resumed iterator's previous match ended at `pos`, inside
            // the subject; a fresh one may start one past its end.
            within(*pos, if *resumed { len } else { len + 1 })
        }
        SavedObject::GmatchEmptyIter { pos, len } => {
            within(*len, crate::limits::MAX_STRING_BYTES)?;
            within(
                *pos,
                crate::saturate::saturating_cast::<usize, u64>(*len) + 1,
            )
        }
        SavedObject::Table { .. } | SavedObject::Closure { .. } => Ok(()),
    }
}

fn validate_references(payload: &SavePayload) -> Result<(), SnapshotError> {
    for value in &payload.upvalues {
        validate_value(value, payload)?;
    }
    for (object_index, object) in payload.objects.iter().enumerate() {
        validate_gmatch(payload, object, object_index)?;
        validate_table_shape(
            payload,
            object,
            object_index,
            &SaveLocation::TableEntries {
                object: object_index,
            },
        )?;
    }
    validate_env_table(
        payload,
        &payload.globals,
        &payload.global_pins,
        0,
        &SaveLocation::Globals,
    )?;
    let mut tokens = BTreeSet::new();
    for (index, record) in payload.env_tables.iter().enumerate() {
        insert_unique(
            &mut tokens,
            record.token.as_str(),
            CorruptionReason::Duplicate {
                entry: DuplicateEntry::EnvironmentToken,
                form: KeyForm::Saved,
            },
            index,
            SaveLocation::EnvironmentTables,
        )?;
        validate_env_table(
            payload,
            &record.table,
            &record.pins,
            index,
            &SaveLocation::EnvironmentTable(index),
        )?;
    }
    Ok(())
}

/// The global table or one environment table: a table in the arena's form,
/// but not in the arena, and its pins.
fn validate_env_table(
    payload: &SavePayload,
    table: &SavedObject,
    pins: &[super::SavedVal],
    index: usize,
    location: &SaveLocation,
) -> Result<(), SnapshotError> {
    if !matches!(table, SavedObject::Table { .. }) {
        return Err(corrupt(
            CorruptionReason::Table(TableCorruption::MaterializedWrongType),
            index,
            location.clone(),
        ));
    }
    validate_table_shape(payload, table, index, location)?;
    // Pins are distinct strings by content, and only a sealed save has any.
    // String arena ids are references, not identities: a forged payload may
    // give equal bytes more than one id.
    let mut seen = BTreeSet::new();
    for (pin_index, pin) in pins.iter().enumerate() {
        let super::SavedVal::Str(id) = pin else {
            return Err(corrupt(
                CorruptionReason::InvalidPin,
                pin_index,
                location.clone(),
            ));
        };
        if !payload.globals_sealed {
            return Err(corrupt(
                CorruptionReason::InvalidPin,
                pin_index,
                location.clone(),
            ));
        }
        validate_value(pin, payload)?;
        // `validate_value` has already checked the id; this lookup only
        // borrows the bytes, so a miss reports the same corruption.
        let Some(content) = payload.strings.get(*id as usize) else {
            return Err(corrupt(
                CorruptionReason::InvalidPin,
                pin_index,
                location.clone(),
            ));
        };
        insert_unique(
            &mut seen,
            content.as_slice(),
            CorruptionReason::Duplicate {
                entry: DuplicateEntry::Pin,
                form: KeyForm::Saved,
            },
            pin_index,
            location.clone(),
        )?;
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
    validate_chunk_runtimes(&payload)?;
    validate_references(&payload)?;
    // gmatch iterators are checked in `validate_references`: the saved
    // record is the iterator, so its state is checkable here.
    validate_pointer_ids(&payload)?;
    Ok(VerifiedSavePayload(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_record(
        token: &str,
        entries: Vec<(super::super::SavedVal, super::super::SavedVal)>,
        pins: Vec<super::super::SavedVal>,
    ) -> super::super::SavedEnvRecord {
        super::super::SavedEnvRecord {
            token: token.to_string(),
            table: SavedObject::Table {
                array_len: 0,
                array_slots: 0,
                entries,
                metatable: None,
            },
            pins,
        }
    }

    fn payload(records: Vec<super::super::SavedEnvRecord>, sealed: bool) -> SavePayload {
        SavePayload {
            strings: vec![b"same".to_vec(), b"same".to_vec()],
            globals_sealed: sealed,
            env_tables: records,
            ..SavePayload::empty(true)
        }
    }

    #[test]
    fn environment_tables_share_the_global_table_checks() {
        use super::super::SavedVal;
        let live = SavedVal::Bool(true);
        assert!(
            verify_payload(payload(
                vec![env_record(
                    "math",
                    vec![(SavedVal::Str(0), live.clone())],
                    vec![SavedVal::Str(0)]
                )],
                true,
            ))
            .is_ok()
        );
        // Invalid keys name the environment table that holds them.
        for key in [SavedVal::Num(f64::NAN.to_bits()), SavedVal::Nil] {
            let entries = vec![(key, SavedVal::Num(1.0f64.to_bits()))];
            assert!(matches!(
                verify_payload(payload(
                    vec![env_record("math", entries, Vec::new())],
                    false
                )),
                Err(SnapshotError::CorruptArena {
                    location: SaveLocation::EnvironmentTable(0),
                    ..
                })
            ));
        }
        // Two saved ids with equal bytes are one runtime key.
        let duplicate_keys = vec![
            (SavedVal::Str(0), live.clone()),
            (SavedVal::Str(1), live.clone()),
        ];
        assert!(matches!(
            verify_payload(payload(
                vec![env_record("math", duplicate_keys, Vec::new())],
                false
            )),
            Err(SnapshotError::CorruptArena {
                location: SaveLocation::EnvironmentTable(0),
                ..
            })
        ));
        // Tokens are unique.
        assert!(matches!(
            verify_payload(payload(
                vec![
                    env_record("math", Vec::new(), Vec::new()),
                    env_record("math", Vec::new(), Vec::new()),
                ],
                false,
            )),
            Err(SnapshotError::CorruptArena {
                location: SaveLocation::EnvironmentTables,
                index: 1,
                ..
            })
        ));
        // Only a sealed save carries pins, and they are distinct strings.
        for (pins, sealed) in [
            (vec![SavedVal::Str(0)], false),
            (vec![SavedVal::Bool(true)], true),
            (vec![SavedVal::Str(0), SavedVal::Str(1)], true),
        ] {
            assert!(matches!(
                verify_payload(payload(vec![env_record("math", Vec::new(), pins)], sealed)),
                Err(SnapshotError::CorruptArena {
                    location: SaveLocation::EnvironmentTable(0),
                    ..
                })
            ));
        }
        // A record is a table.
        let mut closure = env_record("math", Vec::new(), Vec::new());
        closure.table = SavedObject::Closure {
            chunk: 0,
            upvalues: Vec::new(),
        };
        assert!(matches!(
            verify_payload(payload(vec![closure], false)),
            Err(SnapshotError::CorruptArena {
                location: SaveLocation::EnvironmentTable(0),
                ..
            })
        ));
    }

    /// A standard-environment payload with one saved string, for tests that
    /// install their own objects.
    fn string_payload() -> SavePayload {
        SavePayload {
            strings: vec![b"x".to_vec()],
            ..SavePayload::empty(true)
        }
    }

    #[test]
    fn saved_table_array_prefix_is_checked() {
        let num = |n: f64| super::super::SavedVal::Num(n.to_bits());
        let with_table =
            |array_len: u64,
             array_slots: u32,
             entries: Vec<(super::super::SavedVal, super::super::SavedVal)>| {
                let mut payload = string_payload();
                payload.objects = vec![super::super::SavedObject::Table {
                    array_len,
                    array_slots,
                    entries,
                    metatable: None,
                }];
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
            let mut payload = string_payload();
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

        let mut record = env_record("math", Vec::new(), Vec::new());
        record.table = SavedObject::Table {
            array_len: 0,
            array_slots: 0,
            entries: Vec::new(),
            metatable: Some(super::super::SavedVal::Obj(0)),
        };
        let mut env = payload(vec![record], false);
        env.objects = vec![super::super::SavedObject::Closure {
            chunk: 0,
            upvalues: Vec::new(),
        }];
        assert!(matches!(
            validate_references(&env),
            Err(SnapshotError::CorruptArena { .. })
        ));
    }

    #[test]
    fn pointer_ids_must_sit_below_the_next_id() {
        let with_next = |next: u64| SavePayload {
            strings: vec![b"x".to_vec()],
            next_format_pointer_id: next,
            format_pointer_ids: vec![(super::super::SavedVal::Str(0), 1)],
            ..SavePayload::empty(true)
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
    fn duplicate_global_names_and_malformed_pins_are_rejected() {
        use super::super::SavedVal::{Bool, Str};
        let with_globals = |entries: Vec<(super::super::SavedVal, super::super::SavedVal)>,
                            pins: Vec<super::super::SavedVal>,
                            sealed: bool| SavePayload {
            strings: vec![b"a".to_vec(), b"b".to_vec()],
            globals: super::super::SavedObject::Table {
                array_len: 0,
                array_slots: 0,
                entries,
                metatable: None,
            },
            global_pins: pins,
            globals_sealed: sealed,
            next_format_pointer_id: 0,
            ..SavePayload::empty(true)
        };
        let one = (Str(0), Bool(true));
        let two = (Str(0), Bool(false));
        let other = (Str(1), Bool(false));
        assert!(
            validate_references(&with_globals(vec![one.clone(), other], vec![], false)).is_ok()
        );
        assert!(matches!(
            validate_references(&with_globals(vec![one.clone(), two], vec![], false)),
            Err(SnapshotError::CorruptArena { .. })
        ));
        // Pins: strings, distinct by content, and only in a sealed save.
        assert!(validate_references(&with_globals(vec![], vec![Str(0), Str(1)], true)).is_ok());
        for (pins, sealed) in [
            (vec![Str(0)], false),
            (vec![Bool(true)], true),
            (vec![Str(0), Str(0)], true),
        ] {
            assert!(matches!(
                validate_references(&with_globals(vec![], pins, sealed)),
                Err(SnapshotError::CorruptArena { .. })
            ));
        }
        let mut duplicate_pin_contents = with_globals(vec![], vec![Str(0), Str(1)], true);
        let content = duplicate_pin_contents.strings[0].clone();
        duplicate_pin_contents.strings[1] = content;
        assert!(matches!(
            validate_references(&duplicate_pin_contents),
            Err(SnapshotError::CorruptArena { .. })
        ));
    }
}
