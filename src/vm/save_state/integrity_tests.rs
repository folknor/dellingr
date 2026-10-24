use super::*;
use std::convert::Infallible;

fn empty_payload() -> SavePayload {
    SavePayload {
        has_standard_environment: false,
        rng_state: 0,
        cost: CostState::default(),
        strings: Vec::new(),
        bytecode: Vec::new(),
        upvalues: Vec::new(),
        objects: Vec::new(),
        globals: SavedObject::empty_table(),
        global_pins: Vec::new(),
        globals_sealed: false,
        env_deltas: Vec::new(),
        next_format_pointer_id: 1,
        format_pointer_ids: Vec::new(),
    }
}

fn empty_delta(token: &str) -> SavedEnvDelta {
    SavedEnvDelta {
        token: token.to_owned(),
        deleted: Vec::new(),
        upserts: Vec::new(),
        order: None,
        array: None,
        hash_slots: None,
        metatable: SavedMetatableDelta::Unchanged,
    }
}

#[test]
fn corrupt_environment_key_names_its_delta_and_section() {
    let mut payload = empty_payload();
    let mut delta = empty_delta("second");
    delta.deleted.push(SavedVal::Nil);
    payload.env_deltas = vec![empty_delta("first"), delta];
    assert!(matches!(
        verify::verify_payload(payload),
        Err(SnapshotError::CorruptArena {
            index: 0,
            location: SaveLocation::EnvironmentDeleted(1),
            reason: CorruptionReason::NilKey,
        })
    ));
}

#[test]
fn corrupt_metatable_names_its_owner_instead_of_zero() {
    let mut payload = empty_payload();
    payload.objects = vec![
        SavedObject::Table {
            array_len: 0,
            array_slots: 0,
            entries: Vec::new(),
            metatable: None,
        },
        SavedObject::Table {
            array_len: 0,
            array_slots: 0,
            entries: Vec::new(),
            metatable: Some(SavedVal::Bool(false)),
        },
    ];
    assert!(matches!(
        verify::verify_payload(payload),
        Err(SnapshotError::CorruptArena {
            index: 1,
            location: SaveLocation::Objects,
            reason: CorruptionReason::Table(TableCorruption::MetatableNotTable),
        })
    ));
}

#[test]
fn saved_keys_share_numeric_equality_and_string_content_equality() {
    let strings = vec![b"same".to_vec(), b"same".to_vec()];
    let location = SaveLocation::TableEntries { object: 3 };
    let key = |value: SavedVal| {
        value
            .table_key(&strings, 2, location.clone())
            .expect("valid test key")
    };
    assert!(key(SavedVal::Num(0.0f64.to_bits())) == key(SavedVal::Num((-0.0f64).to_bits())));
    assert!(key(SavedVal::Str(0)) == key(SavedVal::Str(1)));
    for invalid in [SavedVal::Nil, SavedVal::Num(f64::NAN.to_bits())] {
        assert!(matches!(
            invalid.table_key(&strings, 2, location.clone()),
            Err(SnapshotError::CorruptArena {
                index: 2,
                location: SaveLocation::TableEntries { object: 3 },
                ..
            })
        ));
    }
}

#[test]
// Per-field coverage of `InFlight` lives in `inventory`'s tests; this checks
// the State-level reset also clears the host-held fields, and leaves nothing
// a save could tell apart from a pristine State.
fn reset_clears_in_flight_and_host_held_state() {
    let mut state = State::new();
    assert!(state.validate_quiescent().is_ok());
    let before = state
        .save_state()
        .expect("pristine state can be saved")
        .bytes;
    state.push_nil().expect("one value fits the test stack");
    state.in_flight.stack_bottom = 1;
    state.in_flight.pending_bytecode_caches.push(1);
    state.in_flight.rooted_values.push(Val::Nil);
    state
        .suspended_envs
        .push(super::super::global_env::SuspendedEnv::detached());
    state.in_flight.metamethod_depth = 1;
    state.in_flight.call_depth = 1;
    state.in_flight.native_call_depth = 1;
    assert_eq!(state.validate_quiescent(), Err(SaveError::NotQuiescent));
    state.reset_in_flight();
    assert!(state.validate_quiescent().is_ok());
    assert_eq!(
        state.save_state().expect("reset state can be saved").bytes,
        before
    );
}

#[test]
fn canonical_environment_tokens_survive_load_without_renaming() {
    let state = State::new();
    let names: BTreeSet<&str> = state.env_tokens.values().map(String::as_str).collect();
    assert_eq!(
        names,
        BTreeSet::from([
            "math",
            "string",
            "table",
            "_G.metatable",
            "string.metatable"
        ])
    );
    let before = state
        .save_state()
        .expect("standard environment saves")
        .bytes;
    let loaded = State::load_state(&before, Box::new(crate::DefaultCallbacks), |_| {
        Ok::<(), Infallible>(())
    })
    .expect("standard environment loads");
    assert_eq!(
        loaded.save_state().expect("loaded environment saves").bytes,
        before
    );
}

#[test]
fn environment_fifo_snapshot_is_compact_and_byte_stable() {
    let mut original = State::new();
    let ptr = *original
        .env_tokens
        .iter()
        .find(|(_, token)| token.as_str() == "math")
        .expect("math has an environment token")
        .0;
    let table = original.heap.as_table(ptr).expect("math is a table");
    for key in 1..=10_000 {
        table
            .insert(Val::Num(f64::from(key)), Val::Bool(true))
            .unwrap();
        table.insert(Val::Num(f64::from(key)), Val::Nil).unwrap();
    }
    table.insert(Val::Num(10_001.0), Val::Bool(true)).unwrap();
    let before = original.save_state().expect("FIFO saves").bytes;
    assert!(
        before.len() < 10_000,
        "discarded prefix must not be encoded"
    );
    let loaded = State::load_state(&before, Box::new(crate::DefaultCallbacks), |_| {
        Ok::<(), Infallible>(())
    })
    .expect("FIFO loads");
    assert_eq!(
        loaded.save_state().expect("restored FIFO saves").bytes,
        before
    );
    let ptr = *loaded
        .env_tokens
        .iter()
        .find(|(_, token)| token.as_str() == "math")
        .expect("loaded math has an environment token")
        .0;
    let table = loaded
        .heap
        .as_table_ref(ptr)
        .expect("loaded math is a table");
    let (len, slots, entries) = table.snapshot_entries();
    assert_eq!(len, 10_001);
    assert_eq!(slots, 1);
    assert!(matches!(entries[0], (Val::Num(10_001.0), Val::Bool(true))));
}

#[test]
fn environment_empty_array_span_retains_logical_length() {
    let mut original = State::new();
    let ptr = *original
        .env_tokens
        .iter()
        .find(|(_, token)| token.as_str() == "math")
        .expect("math has an environment token")
        .0;
    let table = original.heap.as_table(ptr).expect("math is a table");
    for key in 1..=10_000 {
        table
            .insert(Val::Num(f64::from(key)), Val::Bool(true))
            .unwrap();
        table.insert(Val::Num(f64::from(key)), Val::Nil).unwrap();
    }
    let before = original.save_state().expect("empty FIFO saves").bytes;
    assert!(
        before.len() < 10_000,
        "empty span must not encode its nil prefix"
    );
    let loaded = State::load_state(&before, Box::new(crate::DefaultCallbacks), |_| {
        Ok::<(), Infallible>(())
    })
    .expect("empty FIFO loads");
    assert_eq!(
        loaded.save_state().expect("empty FIFO resaves").bytes,
        before
    );
}

#[test]
fn environment_array_span_rejects_noncanonical_or_inconsistent_shapes() {
    let num = |n: f64| SavedVal::Num(n.to_bits());
    let live = SavedVal::Bool(true);
    let mut valid = empty_delta("math");
    valid.array = Some((1000, vec![(num(1000.0), live.clone())]));
    valid.upserts = vec![(num(1000.0), live.clone())];
    let payload = |delta| {
        let mut payload = empty_payload();
        payload.env_deltas.push(delta);
        payload
    };
    assert!(verify::verify_payload(payload(valid.clone())).is_ok());
    let mut omitted = valid.clone();
    omitted.upserts.push((num(999.0), live.clone()));
    let mut wrong_key = valid.clone();
    wrong_key.array = Some((1000, vec![(num(1001.0), live.clone())]));
    let mut nil_endpoint = valid.clone();
    nil_endpoint.array = Some((1000, vec![(num(1000.0), SavedVal::Nil)]));
    let mut too_long = valid;
    too_long.array = Some((u64::MAX, Vec::new()));
    for delta in [omitted, wrong_key, nil_endpoint, too_long] {
        assert!(matches!(
            verify::verify_payload(payload(delta)),
            Err(SnapshotError::CorruptArena { .. })
        ));
    }
}

fn folded_host_fn(_: &mut State) -> crate::Result<u8> {
    Ok(0)
}

/// Encode `payload` as a save and load it into a State whose setup registers
/// `folded_host_fn` under both `host.a` and `host.b`, as a build that folds
/// two functions the saving build kept apart would.
fn load_into_folding_build(payload: &SavePayload) -> Result<State, LoadError<SaveError>> {
    let bytes = encode_fixture(payload);
    State::load_state(&bytes, Box::new(crate::DefaultCallbacks), |state| {
        state.register_rust_fn("host.a", folded_host_fn)?;
        state.register_rust_fn("host.b", folded_host_fn)
    })
}

fn encode_fixture(payload: &SavePayload) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.write_magic_and_versions();
    encoder
        .write_bytes(
            env!("CARGO_PKG_VERSION").as_bytes(),
            SnapshotField::EngineVersion,
        )
        .and_then(|()| payload.encode(&mut encoder))
        .expect("fixture payload encodes");
    encoder.finish()
}

#[test]
fn folded_functions_keep_the_smaller_rendered_identity() {
    let mut payload = empty_payload();
    payload.next_format_pointer_id = 3;
    payload.format_pointer_ids = vec![
        (SavedVal::Fn("host.b".to_owned()), 2),
        (SavedVal::Fn("host.a".to_owned()), 1),
    ];
    let mut loaded = load_into_folding_build(&payload).expect("folded identities load");
    assert_eq!(loaded.next_format_pointer_id, 3);
    loaded.push_val(Val::RustFn(folded_host_fn)).unwrap();
    assert_eq!(loaded.format_pointer_id(-1).unwrap(), Some(1));
    assert_eq!(loaded.format_pointer_ids.len(), 1);
}

#[test]
fn folded_functions_as_keys_of_one_table_are_rejected() {
    let mut payload = empty_payload();
    payload.objects = vec![SavedObject::Table {
        array_len: 0,
        array_slots: 0,
        entries: vec![
            (SavedVal::Fn("host.a".to_owned()), SavedVal::Bool(true)),
            (SavedVal::Fn("host.b".to_owned()), SavedVal::Bool(true)),
        ],
        metatable: None,
    }];
    assert!(matches!(
        load_into_folding_build(&payload),
        Err(LoadError::Snapshot(SnapshotError::CorruptArena {
            reason: CorruptionReason::Duplicate {
                entry: DuplicateEntry::TableKey,
                form: KeyForm::Materialized,
            },
            ..
        }))
    ));
}
