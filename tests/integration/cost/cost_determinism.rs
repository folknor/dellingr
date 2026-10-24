//! Regression coverage for budget accounting and saved gmatch iterator costs.

use dellingr::State;
use dellingr::limits::PATTERN_CACHE_ENTRIES;

use crate::support::{self, run};

fn call_iterator(state: &mut State, name: &str) -> (u64, Vec<u8>) {
    let before = state.cost_used();
    support::eval(state, format!("return {name}()"), 1);
    let bytes = state
        .to_bytes(-1)
        .expect("iterator returns a string")
        .to_vec();
    state.pop(1).expect("remove iterator result");
    (state.cost_used() - before, bytes)
}

#[test]
fn unbudgeted_bytecode_and_native_work_keep_an_unbounded_balance() {
    let mut state = State::new();
    for source in [
        "local a = 1; local b = 2; return a + b",
        "return math.sin(1)",
        "return string.upper('abc')",
        "return table.move({1, 2}, 1, 2, 1)",
    ] {
        let before = state.cost_used();
        run(&mut state, source);
        assert!(state.cost_used() > before);
        assert_eq!(state.cost_remaining(), i64::MAX);
    }
}

#[test]
fn gmatch_cost_ignores_collection_between_iterations() {
    fn trace(collect: bool) -> Vec<(u64, Vec<u8>)> {
        let mut state = State::new();
        run(
            &mut state,
            "iter = string.gmatch('one two three four', '%a+')",
        );
        (0..4)
            .map(|_| {
                if collect {
                    state.gc_collect();
                }
                call_iterator(&mut state, "iter")
            })
            .collect()
    }
    assert_eq!(trace(false), trace(true));
}

#[test]
fn gmatch_first_use_cost_ignores_a_shared_pattern_cache_hit() {
    let mut state = State::new();
    state.gc_disable_auto();
    run(
        &mut state,
        "a = string.gmatch('one two', '%a+'); b = string.gmatch('one two', '%a+')",
    );
    assert_eq!(
        call_iterator(&mut state, "a"),
        call_iterator(&mut state, "b")
    );
}

#[test]
fn gmatch_resume_cost_ignores_fifo_cache_eviction() {
    let mut state = State::new();
    state.gc_disable_auto();
    run(&mut state, "iter = string.gmatch('one two', '%a+')");
    call_iterator(&mut state, "iter");
    #[cfg(feature = "snapshot")]
    let saved = state.save_state().expect("iterator saves");
    for i in 0..PATTERN_CACHE_ENTRIES {
        run(
            &mut state,
            format!("local other = string.gmatch('x', 'p{i}'); other()"),
        );
    }
    let evicted = call_iterator(&mut state, "iter");
    let mut fresh = State::new();
    fresh.gc_disable_auto();
    run(&mut fresh, "iter = string.gmatch('one two', '%a+')");
    call_iterator(&mut fresh, "iter");
    assert_eq!(evicted, call_iterator(&mut fresh, "iter"));
    #[cfg(feature = "snapshot")]
    {
        let mut loaded = State::load_state(
            &saved.bytes,
            Box::new(dellingr::DefaultCallbacks),
            support::no_setup,
        )
        .expect("iterator loads");
        assert_eq!(evicted, call_iterator(&mut loaded, "iter"));
    }
}

#[cfg(feature = "snapshot")]
#[test]
fn gmatch_snapshot_preserves_first_use_and_resume_charges() {
    for warm in [false, true] {
        let mut original = State::new();
        original.set_cost_budget(10_000);
        run(&mut original, "iter = string.gmatch('one two', '%a+')");
        if warm {
            call_iterator(&mut original, "iter");
        }
        let saved = original.save_state().expect("iterator saves");
        let mut loaded = State::load_state(
            &saved.bytes,
            Box::new(dellingr::DefaultCallbacks),
            support::no_setup,
        )
        .expect("iterator loads");
        assert_eq!(
            call_iterator(&mut original, "iter"),
            call_iterator(&mut loaded, "iter")
        );
        assert_eq!(original.cost_used(), loaded.cost_used());
        assert_eq!(original.cost_remaining(), loaded.cost_remaining());
    }
}
