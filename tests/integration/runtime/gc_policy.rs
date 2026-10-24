use dellingr::State;

#[test]
fn custom_gc_policy_is_used_before_and_after_state_construction() {
    let initial_threshold = 1_000_000;
    let minimum_threshold = 7;
    let growth_factor = 3;
    let mut state = State::with_gc_policy(initial_threshold, minimum_threshold, growth_factor);

    assert_eq!(state.gc_threshold(), initial_threshold);
    assert!(!state.gc_should_run());

    state.gc_collect();
    let survivors = state.heap_size();
    assert_eq!(
        state.gc_threshold(),
        survivors
            .saturating_mul(growth_factor)
            .max(minimum_threshold)
    );
}

#[test]
fn host_threshold_survives_collection_and_auto_mode_can_resume() {
    let initial_threshold = 1_000_000;
    let minimum_threshold = 11;
    let growth_factor = 4;
    let mut state = State::with_gc_policy(initial_threshold, minimum_threshold, growth_factor);

    let host_threshold = 2_000_000;
    state.gc_set_threshold(host_threshold);
    state.gc_collect();
    assert_eq!(state.gc_threshold(), host_threshold);

    state.gc_set_auto_threshold();
    assert_eq!(state.gc_threshold(), initial_threshold);
    state.gc_collect();
    let survivors = state.heap_size();
    assert_eq!(
        state.gc_threshold(),
        survivors
            .saturating_mul(growth_factor)
            .max(minimum_threshold)
    );
}

#[cfg(feature = "snapshot")]
#[test]
fn threshold_set_during_load_setup_survives_materialization_collection() {
    use std::convert::Infallible;

    use dellingr::DefaultCallbacks;

    let original = State::new();
    let save = original.save_state().expect("new state saves");
    let host_threshold = 50_000;
    let loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        state.gc_set_threshold(host_threshold);
        Ok::<(), Infallible>(())
    })
    .expect("state loads");

    assert_eq!(loaded.gc_threshold(), host_threshold);
}
