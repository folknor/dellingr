use dellingr::State;

#[test]
fn custom_gc_policy_sets_initial_and_recomputed_thresholds() {
    let initial_threshold = 1_000_000;
    let minimum_threshold = 7;
    let growth_factor = 3;
    let mut state = State::builder()
        .gc_policy(initial_threshold, minimum_threshold, growth_factor)
        .build();

    assert_eq!(state.gc_threshold(), initial_threshold);
    assert!(!state.gc_should_run());

    state.gc_collect();
    let survivors = state.gc_score();
    assert_eq!(
        state.gc_threshold(),
        survivors
            .saturating_mul(growth_factor)
            .max(minimum_threshold)
    );
}

#[test]
fn minimum_threshold_wins_when_growth_would_be_smaller() {
    let mut state = State::raw();
    state.gc_collect();
    let survivors = state.gc_score();
    let growth_factor = 2;
    let minimum_threshold = survivors
        .checked_mul(growth_factor)
        .and_then(|growth_threshold| growth_threshold.checked_add(1))
        .expect("raw state heap leaves room for a larger minimum threshold");

    state.gc_set_policy(minimum_threshold, minimum_threshold, growth_factor);
    state.gc_collect();

    assert_eq!(state.gc_score(), survivors);
    assert_eq!(state.gc_threshold(), minimum_threshold);
}

#[test]
fn host_threshold_survives_collection_and_auto_mode_can_resume() {
    let initial_threshold = 1_000_000;
    let minimum_threshold = 11;
    let growth_factor = 4;
    let mut state = State::builder()
        .gc_policy(initial_threshold, minimum_threshold, growth_factor)
        .build();

    let host_threshold = 2_000_000;
    state.gc_set_threshold(host_threshold);
    state.gc_collect();
    assert_eq!(state.gc_threshold(), host_threshold);

    state.gc_set_auto_threshold();
    assert_eq!(state.gc_threshold(), initial_threshold);
    state.gc_collect();
    let survivors = state.gc_score();
    assert_eq!(
        state.gc_threshold(),
        survivors
            .saturating_mul(growth_factor)
            .max(minimum_threshold)
    );
}

#[test]
fn resident_payload_counts_toward_the_automatic_score_only() {
    let mut state = State::new();
    state.gc_collect();
    let before = state.gc_score();
    let objects_before = state.heap_size();
    crate::support::eval(
        &mut state,
        "big = {} for i = 1, 100000 do big[i] = i end",
        0,
    );
    state.gc_collect();
    // 100000 slots of 16 bytes are at least 6250 units of 256 bytes.
    if state.gc_score() < before + 6_000 {
        panic!("score {} did not count the array backing", state.gc_score());
    }
    if state.heap_size() > objects_before + 8 {
        panic!("heap_size counts payload: {}", state.heap_size());
    }
    assert_eq!(state.gc_threshold(), state.gc_score() * 2);

    // A host-set threshold keeps counting allocations, not payload.
    state.gc_set_threshold(state.heap_size() + 1);
    assert!(!state.gc_should_run());
}

#[test]
fn growth_factors_zero_and_one_leave_one_unit_of_headroom() {
    for growth_factor in [0, 1] {
        let mut state = State::builder().gc_policy(0, 0, growth_factor).build();
        state.gc_collect();
        assert_eq!(state.gc_threshold(), state.gc_score() + 1);
        // An unchanged heap, as after a rooted intern hit, does not collect.
        assert!(!state.gc_should_run());
    }
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

#[test]
fn back_edges_reclaim_staged_table_backing_without_new_allocations() {
    let mut state = State::builder().gc_policy(512, 512, 2).build();
    crate::support::eval(
        &mut state,
        r"
        local staged = {}
        for i = 1, 64 do staged[i] = {} end

        for i = 1, 64 do
            local t = staged[i]
            for j = 1, 4096 do t[j] = j end
            staged[i] = nil
        end
        ",
        0,
    );
    // Without back-edge collection, all 64 payloads are still resident,
    // contributing at least 64 * 4096 / 16 = 16384 units on their own.
    assert!(
        state.gc_score() < 2_048,
        "retained score: {}",
        state.gc_score()
    );
}

#[test]
fn tail_call_admission_services_pressure_and_keeps_callee_and_arguments_rooted() {
    let mut state = State::new();
    crate::support::run(&mut state, "t = {} for i = 1, 1024 do t[i] = i end");
    crate::support::load(
        &mut state,
        r"
        local function finish(arg) return arg[1025] end
        local value = t
        t = nil
        value[1025] = 42
        return finish(value)
        ",
    );
    state.gc_collect();
    // Leave room for the closure. The table growth arms the checkpoint.
    let initial = state.gc_score() + 4;
    state.gc_set_policy(initial, initial, 2);

    state
        .call(dellingr::ArgCount::Fixed(0), dellingr::RetCount::Fixed(1))
        .expect("tail call runs after the collection");
    assert!(state.gc_threshold() > initial);
    assert_eq!(
        state.to_number(1).expect("finish returns the new entry"),
        42.0
    );
}
