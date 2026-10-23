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
        user_globals: Vec::new(),
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
fn reset_and_quiescence_share_the_snapshot_inventory() {
    let mut state = State::new();
    assert!(state.validate_quiescent().is_ok());
    let before = state
        .save_state()
        .expect("pristine state can be saved")
        .bytes;
    state.push_nil().expect("one value fits the test stack");
    state.stack_bottom = 1;
    state.pending_bytecode_caches.push(1);
    state.transient_roots.values.push(Val::Nil);
    state
        .transient_roots
        .suspended_envs
        .push(super::super::global_env::GlobalEnv::new());
    state.metamethod_depth = 1;
    state.call_depth = 1;
    state.native_call_depth = 1;
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
            "_G",
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
