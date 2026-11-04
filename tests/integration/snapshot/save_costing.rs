//! A save carries the State's costing next to its counters, and a load builds
//! the State under that costing before running setup.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dellingr::snapshot::DecodeReason;
use dellingr::{Charge, CostModel, Costing, DefaultCallbacks, LoadError, SnapshotError, State};

use crate::support;

/// Executes exactly one `+`.
const ONE_ADD: &str = "local a = 1\nlocal b = a + a";

fn custom_model() -> CostModel {
    CostModel::default()
        .with(Charge::Add, 7)
        .with(Charge::Pow, 0)
        .with(Charge::TableSort, 123)
}

fn metered_state(model: CostModel) -> State {
    State::builder().costing(Costing::Metered(model)).build()
}

/// What `ONE_ADD` costs under `model` from a fresh budget.
fn one_add_cost(model: CostModel) -> u64 {
    let mut state = metered_state(model);
    state.set_cost_budget(1_000_000).unwrap();
    support::run(&mut state, ONE_ADD);
    state.cost_used()
}

/// Byte offset of the costing tag in a save: the header (magic, format word,
/// cost-model word), the engine version string, the standard-library flag and
/// the RNG word.
fn costing_tag_offset() -> usize {
    8 + 4 + env!("CARGO_PKG_VERSION").len() + 1 + 8
}

fn load(bytes: &[u8]) -> Result<State, LoadError<Infallible>> {
    State::load_state(bytes, Box::new(DefaultCallbacks), support::no_setup)
}

fn decode_failure(bytes: &[u8]) -> (usize, DecodeReason) {
    match load(bytes) {
        Err(LoadError::Snapshot(SnapshotError::DecodeError { offset, reason })) => (offset, reason),
        Err(other) => panic!("expected a decode error, got {other:?}"),
        Ok(_) => panic!("the edited save must not load"),
    }
}

#[test]
fn metered_custom_weights_and_counters_round_trip() {
    let model = custom_model();
    let mut original = metered_state(model.clone());
    original.set_cost_budget(1000).unwrap();
    support::run(&mut original, ONE_ADD);
    original.consume_cost(5).unwrap();
    let used = original.cost_used();
    let remaining = original.cost_remaining();
    assert!(used > 0);

    let save = original.save_state().expect("metered state saves");
    let loaded = load(&save.bytes).expect("metered save loads");
    assert_eq!(loaded.costing(), &Costing::Metered(model));
    assert_eq!(loaded.cost_used(), used);
    assert_eq!(loaded.cost_remaining(), remaining);
    assert_eq!(loaded.save_state().unwrap().bytes, save.bytes);
}

#[test]
fn default_costing_round_trips_as_default() {
    let save = State::new().save_state().unwrap();
    let loaded = load(&save.bytes).unwrap();
    assert_eq!(loaded.costing(), &Costing::default());
}

#[test]
fn off_round_trips_and_still_refuses_a_budget() {
    let mut original = State::builder().costing(Costing::Off).build();
    support::run(&mut original, ONE_ADD);
    assert_eq!(original.cost_used(), 0);
    let save = original.save_state().expect("an Off state saves");

    let mut loaded = load(&save.bytes).expect("an Off save loads");
    assert_eq!(loaded.costing(), &Costing::Off);
    assert_eq!(loaded.cost_used(), 0);
    assert_eq!(loaded.cost_remaining(), i64::MAX);
    assert!(loaded.set_cost_budget(10).is_err());
    support::run(&mut loaded, ONE_ADD);
    assert_eq!(loaded.cost_used(), 0);
    assert_eq!(loaded.save_state().unwrap().bytes, save.bytes);
}

#[test]
fn setup_runs_under_the_saved_weights_and_counters_are_restored() {
    let model = custom_model();
    let default_cost = one_add_cost(CostModel::default());
    let custom_cost = one_add_cost(model.clone());
    assert_eq!(custom_cost - default_cost, 6, "one add at weight 7 vs 1");

    let mut original = metered_state(model);
    original.set_cost_budget(500).unwrap();
    support::run(&mut original, ONE_ADD);
    let used = original.cost_used();
    let remaining = original.cost_remaining();
    let save = original.save_state().unwrap();

    let setup_cost = Arc::new(AtomicU64::new(0));
    let observed = Arc::clone(&setup_cost);
    let loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), move |state| {
        assert_eq!(state.cost_used(), 0, "setup starts with fresh counters");
        assert_eq!(state.cost_remaining(), i64::MAX, "setup starts unbudgeted");
        state.set_cost_budget(1_000_000).unwrap();
        support::run(state, ONE_ADD);
        observed.store(state.cost_used(), Ordering::SeqCst);
        Ok::<(), Infallible>(())
    })
    .expect("load with a metering setup");

    assert_eq!(setup_cost.load(Ordering::SeqCst), custom_cost);
    assert_eq!(loaded.cost_used(), used);
    assert_eq!(loaded.cost_remaining(), remaining);
}

#[test]
fn setup_of_an_off_save_cannot_set_a_budget() {
    let save = State::builder()
        .costing(Costing::Off)
        .build()
        .save_state()
        .unwrap();
    let refused = Arc::new(AtomicU64::new(0));
    let observed = Arc::clone(&refused);
    State::load_state(&save.bytes, Box::new(DefaultCallbacks), move |state| {
        observed.store(
            u64::from(state.set_cost_budget(10).is_err()),
            Ordering::SeqCst,
        );
        Ok::<(), Infallible>(())
    })
    .unwrap();
    assert_eq!(refused.load(Ordering::SeqCst), 1);
}

#[test]
fn unknown_costing_tag_is_rejected_at_its_offset() {
    let mut bytes = State::new().save_state().unwrap().bytes;
    let tag = costing_tag_offset();
    bytes[tag] = 9;
    assert_eq!(
        decode_failure(&bytes),
        (tag, DecodeReason::InvalidCostingTag(9))
    );
}

#[test]
fn truncated_weight_list_is_rejected() {
    let bytes = State::new().save_state().unwrap().bytes;
    let tag = costing_tag_offset();
    let kept = tag + 1 + 4 * (Charge::ALL.len() - 1);
    // Two bytes of the last weight survive: the read of that weight fails.
    assert_eq!(
        decode_failure(&bytes[..kept + 2]),
        (kept, DecodeReason::TruncatedPayload)
    );
    // The list ends exactly after the previous weight.
    assert_eq!(
        decode_failure(&bytes[..kept]),
        (kept, DecodeReason::TruncatedPayload)
    );
}

#[test]
fn off_save_with_use_or_a_budget_is_rejected() {
    let bytes = State::builder()
        .costing(Costing::Off)
        .build()
        .save_state()
        .unwrap()
        .bytes;
    let counters = costing_tag_offset() + 1;
    // Counters are remaining, limit, configured, used.
    let configured = counters + 16;
    let used = configured + 1;

    let mut with_use = bytes.clone();
    with_use[used] = 1;
    assert_eq!(
        decode_failure(&with_use),
        (counters, DecodeReason::InconsistentCostMeter)
    );

    let mut with_budget = bytes;
    with_budget[configured] = 1;
    with_budget[counters..counters + 8].copy_from_slice(&10_i64.to_le_bytes());
    with_budget[counters + 8..counters + 16].copy_from_slice(&10_i64.to_le_bytes());
    assert_eq!(
        decode_failure(&with_budget),
        (counters, DecodeReason::InconsistentCostMeter)
    );
}

#[test]
fn setup_can_reject_untrusted_costing_before_materialization() {
    let mut zero_weights = State::new().save_state().unwrap().bytes;
    let first_weight = costing_tag_offset() + 1;
    zero_weights[first_weight..first_weight + 4 * Charge::ALL.len()].fill(0);

    // A budget reset does not restore the host's prices.
    let mut loaded = load(&zero_weights).unwrap();
    loaded.set_cost_budget(1).unwrap();
    support::run(&mut loaded, ONE_ADD);
    assert_eq!(loaded.cost_used(), 0);
    assert_eq!(loaded.cost_remaining(), 1);

    let off = State::builder()
        .costing(Costing::Off)
        .build()
        .save_state()
        .unwrap()
        .bytes;
    let expected = Costing::default();
    for bytes in [&zero_weights, &off] {
        let result = State::load_state(bytes, Box::new(DefaultCallbacks), |state| {
            if state.costing() != &expected {
                return Err("unexpected saved costing");
            }
            Ok(())
        });
        assert!(matches!(
            result,
            Err(LoadError::Setup("unexpected saved costing"))
        ));
    }

    let bytes = State::new().save_state().unwrap().bytes;
    let accepted = State::load_state(&bytes, Box::new(DefaultCallbacks), |state| {
        if state.costing() != &expected {
            return Err("unexpected saved costing");
        }
        Ok(())
    });
    assert!(accepted.is_ok());
}
