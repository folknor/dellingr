//! Regression coverage for the per-Bytecode runtime cache (shared literal
//! interning + shared inline caches).

use dellingr::{ArgCount, Engine, LuaType, RetCount, State};

fn run(state: &mut State, source: &str) {
    state.load_string(source).expect("test source compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .unwrap_or_else(|error| panic!("test source fails: {source}\n{error}"));
}

fn global_num(state: &mut State, name: &str) -> f64 {
    state.get_global(name).expect("global exists");
    let value = state.to_number(-1).expect("global is numeric");
    state.pop(1).expect("value was pushed");
    value
}

/// Shared monomorphic IC slots must stay correct when closures produced by
/// one factory alternate over different receivers, mutate table layouts,
/// swap globals, and collect in between - a cache mismatch must fall back
/// and repopulate, never return a stale value.
#[test]
fn factory_closures_share_caches_without_semantic_drift() {
    let mut state = State::new();
    run(
        &mut state,
        r#"
            function make_reader()
                return function(t) return t.v end
            end
            r1 = make_reader()
            r2 = make_reader()
            t1 = { v = 1 }
            t2 = { v = 2 }
            checks = 0
            for i = 1, 50 do
                if r1(t1) == 1 and r2(t2) == 2 and r1(t2) == 2 and r2(t1) == 1 then
                    checks = checks + 1
                end
            end
        "#,
    );
    assert_eq!(global_num(&mut state, "checks"), 50.0);

    state.gc_collect();

    // Layout drift: append to one receiver, tombstone the other's key, and
    // verify the shared slots re-validate rather than serve stale entries.
    run(
        &mut state,
        r#"
            t1.w = 10
            t2.v = nil
            drift_ok =
                (r1(t1) == 1 and r2(t1) == 1 and r1(t2) == nil and r2(t2) == nil)
                and 1 or 0
        "#,
    );
    assert_eq!(global_num(&mut state, "drift_ok"), 1.0);

    state.gc_collect();

    // Receiver churn: the readers keep working after their table is rebound.
    run(
        &mut state,
        r#"
            t1 = { v = 7 }
            rebind_ok = (r1(t1) == 7 and r2(t1) == 7) and 1 or 0
        "#,
    );
    assert_eq!(global_num(&mut state, "rebind_ok"), 1.0);

    // Genuine globals_version transition: a global-reading closure warmed
    // before a restricted-env swap must miss inside the sandbox (its shared
    // global IC entry is version-stale there) and revalidate after restore.
    run(
        &mut state,
        r#"
            G = 5
            function make_g() return function() return G end end
            g1 = make_g()
            gv = g1()
        "#,
    );
    assert_eq!(global_num(&mut state, "gv"), 5.0);
    let restricted_miss = state.with_restricted_env(&["g1"], |state| {
        state.get_global("g1").expect("g1 is whitelisted");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(1))
            .expect("g1 runs inside the restricted env");
        let is_nil = state.typ(-1) == LuaType::Nil;
        state.pop(1).expect("result was pushed");
        is_nil
    });
    assert!(
        restricted_miss,
        "warmed global IC must not leak into the sandbox"
    );
    run(&mut state, "swap_ok = (g1() == 5) and 1 or 0");
    assert_eq!(global_num(&mut state, "swap_ok"), 1.0);
}

/// Runtime entries are State-local and reclaimed by reachability: a State
/// that churns through unique chunks returns to its interning baseline after
/// two collections (entries drop on the first, their literals - roots for
/// that same collection - on the second), even while the host retains every
/// compiled `Program` and a second State keeps one loaded.
#[test]
fn repeated_loads_release_runtime_entries_while_programs_stay_retained() {
    let engine = Engine::new();
    let mut primary = engine.new_state();
    primary.gc_collect();
    primary.gc_collect();
    let strings_baseline = primary.string_count();

    let mut retained = Vec::new();
    for i in 0..10 {
        let source = format!("local probe = 'unique_literal_{i}' return probe == probe");
        let program = engine.compile(&source).expect("program compiles");
        primary.load(&program).expect("program loads");
        primary
            .call(ArgCount::Fixed(0), RetCount::Fixed(1))
            .expect("program runs");
        primary.pop(1).expect("result was pushed");
        retained.push(program);
    }

    // The same bytecode also lives in a second State as a live closure;
    // reclamation in `primary` must be blind to both forms of ownership.
    let mut secondary = engine.new_state();
    secondary
        .load(retained.last().expect("programs were retained"))
        .expect("program loads into a second State");

    primary.gc_collect();
    primary.gc_collect();
    assert_eq!(primary.string_count(), strings_baseline);

    secondary
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("second State still runs the shared program");
}

/// Neither runtime-cache population (literal interning, cache construction)
/// nor warm cache hits may charge cost: the charged total for a fixed source
/// must be identical on a cold first execution and on a re-parsed second
/// execution in the same State.
///
/// Hand count for the source below (creation and writes cost 1, everything
/// else is free): two one-field constructors (2 x 2) + two calls each doing
/// one field write and one two-field constructor (2 x 4) = 12.
#[test]
fn runtime_cache_population_and_hits_charge_nothing() {
    const SOURCE: &str = r#"
        local a = { x = 1 }
        local b = { x = 2 }
        local function f(t)
            t.y = 3
            return { p = 4, q = 5 }
        end
        f(a)
        f(b)
    "#;

    let mut state = State::new();
    run(&mut state, SOURCE);
    let cold = state.cost_used();
    assert_eq!(cold, 12);

    // A second execution re-parses (a fresh Bytecode identity) while the
    // State already carries warm entries for identical code; the charged
    // total must not move either way.
    run(&mut state, SOURCE);
    assert_eq!(state.cost_used(), cold * 2);
}

/// A load rejected by the stack cap must leave the string pool and object
/// heap untouched: the stack preflight runs before runtime-cache population,
/// so a failed push interns nothing.
#[test]
fn rejected_load_leaves_string_pool_unchanged() {
    let mut state = State::new();

    // Fill the value stack to the cap.
    while state.push_number(0.0).is_ok() {}

    let strings_before = state.string_count();
    let objects_before = state.object_count();

    let result = state.load_string("local marker_literal = 'cap_probe' return 1");
    assert!(result.is_err(), "load must be rejected at the stack cap");
    assert_eq!(state.string_count(), strings_before);
    assert_eq!(state.object_count(), objects_before);
}
