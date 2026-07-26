#![cfg(feature = "snapshot")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dellingr::{
    ArgCount, DefaultCallbacks, HostCallbacks, LoadError, LuaType, RetCount, SaveError, State,
};

#[derive(Clone, Default)]
struct Capture {
    lines: Arc<Mutex<Vec<String>>>,
}

impl HostCallbacks for Capture {
    fn on_print(&mut self, _source: Option<&str>, _line: u32, message: &str) {
        self.lines.lock().unwrap().push(message.to_string());
    }
}

fn run(state: &mut State, source: &str) {
    state.load_string(source).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
}

#[test]
fn deep_global_chain_saves_and_loads_iteratively() {
    let mut state = State::new();
    state.gc_disable_auto();
    run(
        &mut state,
        "deep = {}\nfor i = 1, 50000 do deep = { next = deep } end",
    );
    let save = state.save_state().expect("deep chain saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {})
        .expect("deep chain loads");
    run(
        &mut loaded,
        "local t = deep\nfor i = 1, 50000 do t = t.next end\ndone = t ~= nil",
    );
}

#[test]
fn global_environment_preserves_non_string_key_identity_across_save_load() {
    let mut state = State::new();
    run(&mut state, "_G[1] = 'number'; _G['1'] = 'string'");
    let save = state.save_state().expect("state saves");
    let mut loaded =
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {}).expect("state loads");
    loaded
        .load_string("return _G[1], _G['1']")
        .expect("query compiles");
    loaded
        .call(ArgCount::Fixed(0), RetCount::Fixed(2))
        .expect("query runs");
    assert_eq!(loaded.to_string(-2).unwrap(), "number");
    assert_eq!(loaded.to_string(-1).unwrap(), "string");
}

fn new_capture_state() -> (State, Arc<Mutex<Vec<String>>>) {
    let callbacks = Capture::default();
    let lines = Arc::clone(&callbacks.lines);
    (State::with_callbacks(Box::new(callbacks)), lines)
}

#[test]
fn round_trip_preserves_data_graph_and_env_references() {
    let setup = r#"
        function make_counter()
            local n = 10
            return function(delta)
                n = n + delta
                return n
            end, function()
                return n
            end
        end

        inc, get = make_counter()
        cyc = {}
        cyc.self = cyc
        cyc.inc = inc
        m = math
        floor = math.floor
    "#;
    let continuation = r#"
        print(get())
        print(inc(5))
        print(get())
        print(cyc.self == cyc)
        print(cyc.inc(1))
        print(floor(3.9))
        print(m.floor(4.9))
    "#;

    let (mut control, control_lines) = new_capture_state();
    run(&mut control, setup);
    run(&mut control, continuation);

    let (mut original, _) = new_capture_state();
    run(&mut original, setup);
    let save = original.save_state().unwrap();
    assert!(!save.bytes.is_empty());

    let (loaded_callbacks, loaded_lines) = {
        let callbacks = Capture::default();
        let lines = Arc::clone(&callbacks.lines);
        (callbacks, lines)
    };
    let mut loaded = State::load_state(&save.bytes, Box::new(loaded_callbacks), |_| {}).unwrap();
    run(&mut loaded, continuation);

    assert_eq!(
        *loaded_lines.lock().unwrap(),
        *control_lines.lock().unwrap()
    );
}

#[test]
fn tombstones_are_omitted_from_snapshots() {
    // Compare the payload against a table that never held the deleted key.
    // Round-trip behavior alone cannot detect a leaked `(b, nil)` entry: loading
    // it would call insert(b, nil), which is remove-semantics on an absent key
    // and gets discarded, so the observable result is identical either way.
    //
    // Compare lengths rather than full bytes. The two scripts execute different
    // numbers of operations, so the serialized cost counters legitimately
    // differ - but those fields are fixed-width, so only a leaked entry can
    // change the payload size.
    // Both storage modes must be covered. A 3-entry table is inline; only a
    // table past INLINE_CAPACITY (4) reaches the IndexMap arm, and the two arms
    // filter dead slots independently.
    for (tombstoned, clean, expected_order) in [
        (
            "t = { a = 1, b = 2, c = 3 }; t.b = nil",
            "t = { a = 1, c = 3 }",
            "acb",
        ),
        (
            "t = { a = 1, b = 2, c = 3, d = 4, e = 5, f = 6 }; t.b = nil",
            "t = { a = 1, c = 3, d = 4, e = 5, f = 6 }",
            "acdefb",
        ),
    ] {
        let (mut with_tombstone, _) = new_capture_state();
        run(&mut with_tombstone, tombstoned);
        let tombstoned_bytes = with_tombstone.save_state().unwrap().bytes;

        let (mut without_tombstone, _) = new_capture_state();
        run(&mut without_tombstone, clean);
        let clean_bytes = without_tombstone.save_state().unwrap().bytes;

        assert_eq!(
            tombstoned_bytes.len(),
            clean_bytes.len(),
            "a tombstoned entry leaked into the snapshot payload for: {tombstoned}"
        );

        // And the live entries still round-trip with insertion order intact,
        // with a reinserted key landing at the back.
        let mut loaded =
            State::load_state(&tombstoned_bytes, Box::new(DefaultCallbacks), |_| {}).unwrap();
        run(
            &mut loaded,
            "t.b = 4; order = ''; for k in pairs(t) do order = order .. k end",
        );
        loaded.get_global("order").unwrap();
        assert_eq!(loaded.to_string(-1).unwrap(), expected_order);
        loaded.pop(1).unwrap();
    }
}

#[test]
fn saves_are_byte_stable() {
    let (mut state, _) = new_capture_state();
    run(
        &mut state,
        r#"
        t = { a = 1, b = "x" }
        t.self = t
        f = string.gsub
    "#,
    );

    let first = state.save_state().unwrap();
    let second = state.save_state().unwrap();
    assert_eq!(first.bytes, second.bytes);
}

#[test]
fn state_is_quiescent_after_error_in_dynamic_call_args() {
    // An error while evaluating a dynamic call's arguments (after the base was
    // marked, before the call ran) must not leave a stale vararg_call_base, or
    // the State would fail quiescence validation and could not be snapshotted
    // (L8).
    let mut state = State::new();
    state
        .load_string(
            r#"
            local function f(...) return ... end
            local function g() return nil + 1 end
            return f(g())
        "#,
        )
        .unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("g() errors mid-call");
    state
        .save_state()
        .expect("state must be quiescent after the killed callback");
}

#[test]
fn state_is_quiescent_after_error_in_vararg_frame() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            local function f(...) return nil + 1 end
            return f({ marker = 42 })
        "#,
        )
        .unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("vararg frame must error");
    state
        .save_state()
        .expect("vararg roots must be released after an error");
}

#[test]
fn state_is_quiescent_after_error_in_table_sort_comparator() {
    let mut state = State::new();
    state
        .load_string(
            r#"
            local t = { { v = 2 }, { v = 1 } }
            table.sort(t, function() return nil + 1 end)
        "#,
        )
        .unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("sort comparator must error");
    state
        .save_state()
        .expect("sort roots must be released after an error");
}

#[test]
fn state_is_quiescent_after_error_in_restricted_environment() {
    let mut state = State::new();
    let result = state.with_restricted_env(&[], |state| {
        state.load_string("return nil + 1")?;
        state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
    });
    result.expect_err("restricted callback must error");
    state
        .save_state()
        .expect("suspended environment must be restored after an error");
}

#[test]
fn save_state_rejects_suspended_environment() {
    let mut state = State::new();
    state.with_restricted_env(&[], |state| {
        assert!(matches!(state.save_state(), Err(SaveError::NotQuiescent)));
    });
}

#[test]
fn state_is_quiescent_after_error_in_table_constructor() {
    // Same guard for the table_constructor_bases stack: an error between
    // NewTableTracked and its SetList must not leak a base (L8).
    let mut state = State::new();
    state
        .load_string(
            r#"
            local function g() return nil + 1 end
            return {g()}
        "#,
        )
        .unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("g() errors mid-constructor");
    state
        .save_state()
        .expect("state must be quiescent after the killed callback");
}

#[test]
fn gmatch_closure_survives_save_load() {
    // A gmatch closure captures the (now library-open-registered) iterator Rust
    // function as an upvalue; persisting and restoring it must resolve that id
    // and resume iteration (L12 snapshot follow-up).
    let mut state = State::new();
    state
        .load_string(
            r#"
            f = string.gmatch("a b c", "%w+")
            return f()
        "#,
        )
        .unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_string(-1).unwrap(), "a");
    state.pop(1).unwrap();

    let save = state.save_state().unwrap();
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {}).unwrap();

    loaded.get_global("f").unwrap();
    loaded.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(loaded.to_string(-1).unwrap(), "b");
}

#[test]
fn empty_gmatch_closure_survives_save_load() {
    // The empty-pattern gmatch path uses a different iterator id; it too must be
    // registered at library-open so its closure restores.
    let mut state = State::new();
    state.load_string(r#"g = string.gmatch("ab", "")"#).unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

    let save = state.save_state().unwrap();
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {}).unwrap();

    loaded.get_global("g").unwrap();
    loaded.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(loaded.to_string(-1).unwrap(), "");
}

#[test]
fn empty_state_round_trip_stays_empty() {
    let state = State::empty();
    let save = state.save_state().unwrap();
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {}).unwrap();

    for name in [
        "print",
        "type",
        "tonumber",
        "tostring",
        "pairs",
        "ipairs",
        "next",
        "getmetatable",
        "setmetatable",
        "rawget",
        "rawset",
        "rawequal",
        "rawlen",
        "select",
        "unpack",
        "math",
        "string",
        "table",
        "_G",
        "error",
    ] {
        loaded.get_global(name).unwrap();
        assert_eq!(loaded.typ(-1), LuaType::Nil, "{name}");
        loaded.pop(1).unwrap();
    }
}

#[test]
fn pairs_iterator_round_trips_after_next_rebinding() {
    let (mut state, _) = new_capture_state();
    run(
        &mut state,
        "iter, iter_state, iter_key = pairs({ a = 7 })\nnext = 42",
    );
    let save = state.save_state().unwrap();
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {}).unwrap();
    loaded
        .load_string("for key, value in iter, iter_state, iter_key do result = value end")
        .unwrap();
    loaded.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
    loaded.get_global("result").unwrap();
    assert_eq!(loaded.to_number(-1).unwrap(), 7.0);
}

#[test]
fn dynamic_table_constructor_round_trips_after_completion() {
    let (mut original, _) = new_capture_state();
    run(
        &mut original,
        r#"
        function values()
            return 2, 3, 5
        end
        t = {1, values()}
    "#,
    );
    let save = original.save_state().unwrap();
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {}).unwrap();
    run(&mut loaded, "a = #t; b = t[1] + t[2] + t[3] + t[4]");
    loaded.get_global("a").unwrap();
    assert_eq!(loaded.to_number(-1).unwrap(), 4.0);
    loaded.pop(1).unwrap();
    loaded.get_global("b").unwrap();
    assert_eq!(loaded.to_number(-1).unwrap(), 11.0);
}

#[test]
fn rng_and_cost_continue_after_load() {
    let (mut control, control_lines) = new_capture_state();
    control.set_rng_seed(99);
    control.set_cost_budget(10);
    control.consume_cost(3).unwrap();
    run(&mut control, "math.random(); math.random()");
    run(
        &mut control,
        r#"
        print(math.random())
        print(math.random(1, 100))
    "#,
    );

    let (mut original, _) = new_capture_state();
    original.set_rng_seed(99);
    original.set_cost_budget(10);
    original.consume_cost(3).unwrap();
    run(&mut original, "math.random(); math.random()");
    let save = original.save_state().unwrap();

    let callbacks = Capture::default();
    let loaded_lines = Arc::clone(&callbacks.lines);
    let mut loaded = State::load_state(&save.bytes, Box::new(callbacks), |_| {}).unwrap();
    assert_eq!(loaded.cost_used(), original.cost_used());
    assert_eq!(loaded.cost_remaining(), original.cost_remaining());
    run(
        &mut loaded,
        r#"
        print(math.random())
        print(math.random(1, 100))
    "#,
    );

    assert_eq!(
        *loaded_lines.lock().unwrap(),
        *control_lines.lock().unwrap()
    );
}

#[test]
fn configured_budget_remains_enforced_after_load() {
    let mut original = State::new();
    run(&mut original, "t = {1, 2, 3, 4, 5}");
    original.set_cost_budget(2);
    let save = original.save_state().expect("state saves");

    let mut loaded =
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {}).expect("state loads");
    assert_eq!(loaded.cost_remaining(), 2);

    // Invoke the native function directly so the saved two units are consumed
    // by table.move itself rather than by bytecode dispatch before the call.
    loaded.get_global("table").unwrap();
    loaded.push_bytes("move").expect("short test string fits");
    loaded.get_table(-2).expect("table.move lookup succeeds");
    loaded.remove(-2).expect("table table is removed");
    loaded.get_global("t").unwrap();
    loaded.push_number(1.0).unwrap();
    loaded.push_number(5.0).unwrap();
    loaded.push_number(2.0).unwrap();
    let error = loaded
        .call(ArgCount::Fixed(4), RetCount::Fixed(1))
        .expect_err("restored configured budget stops table.move");

    assert!(matches!(
        error.kind,
        dellingr::error::ErrorKind::BudgetExceeded { .. }
    ));
    assert_eq!(loaded.cost_remaining(), 0);

    loaded.get_global("t").unwrap();
    for (index, expected) in [1.0, 2.0, 3.0, 4.0, 4.0, 5.0].into_iter().enumerate() {
        loaded.push_number((index + 1) as f64).unwrap();
        loaded.get_table(-2).expect("table read succeeds");
        assert_eq!(
            loaded.to_number(-1).expect("table value is numeric"),
            expected
        );
        loaded.pop(1).unwrap();
    }
}

#[test]
fn unregistered_reachable_rust_function_fails_save() {
    fn host_fn(_state: &mut State) -> dellingr::Result<u8> {
        Ok(0)
    }

    let mut state = State::new();
    state.push_rust_fn(host_fn).unwrap();
    state.set_global("host_fn");

    let err = state.save_state().unwrap_err();
    assert!(matches!(
        err,
        SaveError::UnregisteredFunction { reachable_from }
            if reachable_from == "global host_fn"
    ));
}

#[test]
fn anchor_count_is_diagnostic_not_persisted() {
    let mut state = State::new();
    state.push_number(1.0).unwrap();
    let anchor = state.anchor().unwrap();

    let save = state.save_state().unwrap();
    assert_eq!(save.diagnostics.anchor_count, 1);

    let loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {}).unwrap();
    assert_eq!(loaded.anchor_count(), 0);

    assert!(state.release_anchor(anchor));
}

#[test]
fn environment_deltas_preserve_mutations_cycles_order_and_metatables() {
    let mut original = fresh();
    run(
        &mut original,
        r#"
        local user = { nested = { value = 42 } }
        math.floor = function(_) return user.nested.value end
        math.ceil = nil
        math.user = user
        user.back = math
        math.first = 1
        math.second = 2
        math.first = nil
        math.first = 3
        setmetatable(math, user)
    "#,
    );
    let mut loaded = reload(&original);
    run(
        &mut loaded,
        r#"
        floor_value = math.floor(9)
        deleted = math.ceil == nil and 1 or 0
        cycle = math.user.back == math and 1 or 0
        meta = getmetatable(math) == math.user and 1 or 0
        order = ""
        for key in pairs(math) do
            if key == "second" or key == "first" then order = order .. key .. "," end
        end
    "#,
    );
    assert_eq!(global_num(&mut loaded, "floor_value"), 42.0);
    assert_eq!(global_num(&mut loaded, "deleted"), 1.0);
    assert_eq!(global_num(&mut loaded, "cycle"), 1.0);
    assert_eq!(global_num(&mut loaded, "meta"), 1.0);
    assert_eq!(global_str(&mut loaded, "order"), "second,first,");
    // A loaded state remains quiescent and can be saved immediately.
    loaded.save_state().expect("loaded state resaves");
}

#[test]
fn pointer_identities_and_counter_survive_save_load() {
    let mut original = fresh();
    run(
        &mut original,
        "t = {}; s = 'reachable'; saved_identity = tostring(t); saved_percent_p = string.format('%p', t); saved_string_p = string.format('%p', s); saved_rust_fn_p = string.format('%p', print); saved_env_child_p = string.format('%p', getmetatable(_G)); dead = {}; tostring(dead); dead = nil",
    );
    let mut loaded = reload(&original);
    run(
        &mut loaded,
        "loaded_identity = tostring(t); loaded_percent_p = string.format('%p', t); loaded_string_p = string.format('%p', s); loaded_rust_fn_p = string.format('%p', print); loaded_env_child_p = string.format('%p', getmetatable(_G)); later = {}; later_identity = tostring(later)",
    );
    assert_eq!(
        global_str(&mut loaded, "loaded_identity"),
        global_str(&mut loaded, "saved_identity")
    );
    assert_eq!(
        global_str(&mut loaded, "loaded_percent_p"),
        global_str(&mut loaded, "saved_percent_p")
    );
    assert_eq!(
        global_str(&mut loaded, "loaded_string_p"),
        global_str(&mut loaded, "saved_string_p")
    );
    assert_eq!(
        global_str(&mut loaded, "loaded_rust_fn_p"),
        global_str(&mut loaded, "saved_rust_fn_p")
    );
    assert_eq!(
        global_str(&mut loaded, "loaded_env_child_p"),
        global_str(&mut loaded, "saved_env_child_p")
    );
    // The collected object's id is intentionally not reused.
    assert_eq!(global_str(&mut loaded, "later_identity"), "table: 0x6");
}

#[test]
fn ordered_environment_replay_preserves_setup_additions_and_metatable() {
    let mut original = fresh();
    // Deleting and re-adding a STOCK key is what forces an explicit order
    // vector: `floor` moves to the end of the live table, whereas plain
    // delete/upsert replay would restore it in its original baseline position.
    // Adding only new keys replays in order naturally and emits no vector, so
    // it would not exercise this path at all.
    run(&mut original, "math.floor = nil; math.floor = 7");
    let save = original.save_state().expect("state saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        run(
            state,
            "math.future = 99; local mt = { setup = true }; setmetatable(math, mt)",
        );
    })
    .expect("state loads with later environment additions");
    run(
        &mut loaded,
        "floor = math.floor; future = math.future; setup_meta = getmetatable(math).setup and 1 or 0; order = ''; for key in pairs(math) do if key == 'floor' or key == 'future' then order = order .. key .. ',' end end",
    );
    assert_eq!(global_num(&mut loaded, "floor"), 7.0);
    // A key the save never saw survives replay instead of failing the load.
    assert_eq!(global_num(&mut loaded, "future"), 99.0);
    // Rebuilding the entry list must not drop a metatable installed by setup.
    assert_eq!(global_num(&mut loaded, "setup_meta"), 1.0);
    // Saved keys keep their saved order; setup-only keys are appended after.
    assert_eq!(global_str(&mut loaded, "order"), "floor,future,");
}

#[test]
fn unordered_environment_replay_places_setup_additions_before_new_keys() {
    // The counterpart to the test above: when replay reproduces the saved order
    // naturally, no order vector is emitted and a setup-added key simply sits
    // where it was inserted - ahead of keys the delta appends. Lua does not
    // specify `pairs` order, so what is pinned here is only that saved keys keep
    // their relative order and the setup addition survives.
    let mut original = fresh();
    run(
        &mut original,
        "math.first = 1; math.second = 2; math.first = nil; math.first = 3",
    );
    let save = original.save_state().expect("state saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        run(state, "math.future = 99");
    })
    .expect("state loads");
    run(
        &mut loaded,
        "future = math.future; order = ''; for key in pairs(math) do if key == 'second' or key == 'first' or key == 'future' then order = order .. key .. ',' end end",
    );
    assert_eq!(global_num(&mut loaded, "future"), 99.0);
    assert_eq!(global_str(&mut loaded, "order"), "future,second,first,");
}

#[test]
fn setup_anchor_survives_load_final_gc() {
    let original = fresh();
    let save = original.save_state().expect("state saves");
    let mut setup_anchor = None;
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        state.push_number(42.0).unwrap();
        setup_anchor = Some(state.anchor().expect("setup anchor succeeds"));
    })
    .expect("state loads");
    loaded
        .push_anchor(setup_anchor.expect("setup produced anchor"))
        .unwrap();
    assert_eq!(loaded.to_number(-1).unwrap(), 42.0);
    loaded.pop(1).unwrap();
}

#[test]
fn unsupported_format_versions_fail_before_setup() {
    let state = fresh();
    let save = state.save_state().expect("state saves");
    for version in [5_u16, 7_u16] {
        let mut bytes = save.bytes.clone();
        bytes[4..6].copy_from_slice(&version.to_le_bytes());
        let setup_ran = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&setup_ran);
        assert!(matches!(
            State::load_state(&bytes, Box::new(DefaultCallbacks), move |_| {
                observed.store(true, Ordering::SeqCst);
            }),
            Err(LoadError::UnsupportedVersion)
        ));
        assert!(!setup_ran.load(Ordering::SeqCst));
    }
}

#[test]
fn unsupported_cost_model_versions_fail_before_setup() {
    let state = fresh();
    let save = state.save_state().expect("state saves");
    for version in [1_u16, 3_u16] {
        let mut bytes = save.bytes.clone();
        bytes[6..8].copy_from_slice(&version.to_le_bytes());
        let setup_ran = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&setup_ran);
        assert!(matches!(
            State::load_state(&bytes, Box::new(DefaultCallbacks), move |_| {
                observed.store(true, Ordering::SeqCst);
            }),
            Err(LoadError::UnsupportedCostModelVersion)
        ));
        assert!(!setup_ran.load(Ordering::SeqCst));
    }
}

#[test]
fn wide_literal_indices_and_uncached_slots_round_trip() {
    let globals = (0..256)
        .map(|n| format!("g{n} = {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let reads = (0..256)
        .map(|n| format!("r = g{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let fields = (0..256)
        .map(|n| format!("t.f{n} = {n}; q = t.f{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!("{globals}\nlocal t = {{}}\n{fields}\n{reads}\nanswer = g255 + t.f255");

    let mut state = State::new();
    run(&mut state, &source);
    let saved = state.save_state().expect("wide bytecode should save");
    let mut loaded = State::load_state(&saved.bytes, Box::new(DefaultCallbacks), |_| {})
        .expect("wide bytecode should load");
    loaded
        .load_string("return answer")
        .expect("loaded state should accept a continuation");
    loaded
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("continuation should run");
    assert_eq!(
        loaded.to_number(-1).expect("answer should be numeric"),
        510.0
    );
}

#[test]
fn non_quiescent_save_errors() {
    let mut state = State::new();
    state.load_string("x = 1").unwrap();

    assert_eq!(state.save_state().unwrap_err(), SaveError::NotQuiescent);
}

#[test]
fn bad_or_truncated_bytes_error_without_panicking() {
    let bad_magic = match State::load_state(b"nope", Box::new(DefaultCallbacks), |_| {}) {
        Ok(_) => panic!("bad magic load should fail"),
        Err(err) => err,
    };
    assert_eq!(bad_magic, LoadError::BadMagic);

    let state = State::new();
    let mut bytes = state.save_state().unwrap().bytes;
    bytes.pop();
    let truncated = match State::load_state(&bytes, Box::new(DefaultCallbacks), |_| {}) {
        Ok(_) => panic!("truncated load should fail"),
        Err(err) => err,
    };
    assert!(matches!(truncated, LoadError::DecodeError(_)));
}

#[test]
fn missing_host_function_registration_errors_on_load() {
    fn host_fn(_state: &mut State) -> dellingr::Result<u8> {
        Ok(0)
    }

    let mut state = State::new();
    state
        .set_global_named_rust_fn("host_fn", "game.host_fn", host_fn)
        .unwrap();
    let save = state.save_state().unwrap();

    let err = match State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {}) {
        Ok(_) => panic!("missing host function load should fail"),
        Err(err) => err,
    };
    assert_eq!(err, LoadError::UnknownFunction("game.host_fn".to_string()));
}

#[test]
fn registered_host_function_reconnects_on_load() {
    fn host_value(state: &mut State) -> dellingr::Result<u8> {
        state.push_number(42.0)?;
        Ok(1)
    }

    let mut state = State::new();
    state
        .set_global_named_rust_fn("host_value", "game.host_value", host_value)
        .unwrap();
    run(&mut state, "saved_host_value = host_value");
    let save = state.save_state().unwrap();

    let callbacks = Capture::default();
    let lines = Arc::clone(&callbacks.lines);
    let mut loaded = State::load_state(&save.bytes, Box::new(callbacks), |state| {
        state
            .set_global_named_rust_fn("host_value", "game.host_value", host_value)
            .unwrap();
    })
    .unwrap();
    run(&mut loaded, "print(saved_host_value())");

    assert_eq!(*lines.lock().unwrap(), vec!["42".to_string()]);
}

#[test]
fn rust_function_save_id_is_independent_of_registration_order() {
    fn host_fn(_state: &mut State) -> dellingr::Result<u8> {
        Ok(0)
    }

    let mut first = State::new();
    first.register_rust_fn("game.z", host_fn).unwrap();
    first.register_rust_fn("game.a", host_fn).unwrap();
    first.push_rust_fn(host_fn).unwrap();
    first.set_global("host_fn");

    let mut second = State::new();
    second.register_rust_fn("game.a", host_fn).unwrap();
    second.register_rust_fn("game.z", host_fn).unwrap();
    second.push_rust_fn(host_fn).unwrap();
    second.set_global("host_fn");

    assert_eq!(
        first.save_state().unwrap().bytes,
        second.save_state().unwrap().bytes
    );
}

// ---------------------------------------------------------------------------
// Helpers for reading back globals after a load.
// ---------------------------------------------------------------------------

fn fresh() -> State {
    State::new()
}

fn reload(state: &State) -> State {
    let save = state.save_state().unwrap();
    State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {}).unwrap()
}

fn global_num(state: &mut State, name: &str) -> f64 {
    state.get_global(name).unwrap();
    let n = state.to_number(-1).unwrap();
    state.pop(1).unwrap();
    n
}

fn global_str(state: &mut State, name: &str) -> String {
    state.get_global(name).unwrap();
    let s = state.to_string_with_meta(-1).unwrap();
    state.pop(1).unwrap();
    s
}

#[test]
fn shadowed_builtins_survive_round_trip() {
    let mut original = fresh();
    // Reassign a builtin function and a whole library table; both live in
    // `globals` under a builtin name but no longer hold the canonical value.
    run(&mut original, "print = 123\nmath = { tag = 7 }");

    let mut loaded = reload(&original);
    run(
        &mut loaded,
        "observed_print = type(print)\nobserved_math = math.tag",
    );
    assert_eq!(global_str(&mut loaded, "observed_print"), "number");
    assert_eq!(global_num(&mut loaded, "observed_math"), 7.0);
}

#[test]
fn captured_env_ref_survives_builtin_shadow() {
    let mut original = fresh();
    run(
        &mut original,
        r#"
        m = math            -- capture the real library before shadowing
        math = { tag = 7 }  -- then shadow the global name
        before = m.floor(3.9)
    "#,
    );
    assert_eq!(global_num(&mut original, "before"), 3.0);

    let mut loaded = reload(&original);
    run(
        &mut loaded,
        "via_captured = m.floor(9.9)\nvia_shadow = math.tag",
    );
    // The captured reference resolves to the rebuilt real library; the shadowed
    // global resolves to the user's table. The env snapshot keeps them distinct.
    assert_eq!(global_num(&mut loaded, "via_captured"), 9.0);
    assert_eq!(global_num(&mut loaded, "via_shadow"), 7.0);
}

#[test]
fn env_object_as_metatable_resolves_after_load() {
    let mut original = fresh();
    run(
        &mut original,
        "t = {}\nsetmetatable(t, math)\nsame = getmetatable(t) == math and 1 or 0",
    );
    assert_eq!(global_num(&mut original, "same"), 1.0);

    let mut loaded = reload(&original);
    run(&mut loaded, "after = getmetatable(t) == math and 1 or 0");
    // The metatable resolves to the *rebuilt* env object, identical to `math`.
    assert_eq!(global_num(&mut loaded, "after"), 1.0);
}

#[test]
fn cyclic_metatable_round_trips() {
    let mut original = fresh();
    run(
        &mut original,
        "t = {}\nt.x = 42\nsetmetatable(t, t)\nt.__index = t",
    );

    let mut loaded = reload(&original);
    run(
        &mut loaded,
        "self_mt = getmetatable(t) == t and 1 or 0\ndirect = t.x",
    );
    assert_eq!(global_num(&mut loaded, "self_mt"), 1.0);
    assert_eq!(global_num(&mut loaded, "direct"), 42.0);
}

#[test]
fn shared_upvalue_mutation_visible_across_closures() {
    let mut original = fresh();
    run(
        &mut original,
        r#"
        function make()
            local v = 0
            return function(x) v = x end, function() return v end
        end
        setv, getv = make()
        setv(5)
    "#,
    );

    let mut loaded = reload(&original);
    run(&mut loaded, "before = getv()\nsetv(99)\nafter = getv()");
    // The carried-over state is observed, and the two closures still share one
    // upvalue cell: mutating through setv is visible through getv.
    assert_eq!(global_num(&mut loaded, "before"), 5.0);
    assert_eq!(global_num(&mut loaded, "after"), 99.0);
}

#[test]
fn deeply_nested_closures_round_trip() {
    let mut original = fresh();
    run(
        &mut original,
        r#"
        function outer(a)
            return function(b)
                return function(c)
                    return function() return a + b + c end
                end
            end
        end
        f = outer(1)(20)(300)
        g = outer(1000)(1)(1)
    "#,
    );

    let mut loaded = reload(&original);
    run(&mut loaded, "r1 = f()\nr2 = g()");
    assert_eq!(global_num(&mut loaded, "r1"), 321.0);
    assert_eq!(global_num(&mut loaded, "r2"), 1002.0);
}

#[test]
fn saved_outer_creates_new_nested_closures() {
    let mut original = fresh();
    run(
        &mut original,
        r#"
        function outer(a)
            return function(b)
                return function(c)
                    return function() return a + b + c end
                end
            end
        end
    "#,
    );

    let mut loaded = reload(&original);
    run(&mut loaded, "result = outer(1)(20)(300)()");
    assert_eq!(global_num(&mut loaded, "result"), 321.0);
}

#[test]
fn upvalue_arena_entries_do_not_alias_during_save() {
    let mut original = fresh();
    run(
        &mut original,
        r#"
        function make()
            local v = 7
            local holder
            local function a() return holder end
            local function b() return v end
            holder = { b = b }
            return a
        end
        root = make()
    "#,
    );

    let mut loaded = reload(&original);
    run(&mut loaded, "result = type(root())");
    assert_eq!(global_str(&mut loaded, "result"), "table");
}

#[test]
fn binary_string_literal_round_trips_through_save() {
    let mut original = fresh();
    run(
        &mut original,
        r#"
        function f()
            return "\255"
        end
    "#,
    );

    let mut loaded = reload(&original);
    run(&mut loaded, "result = f()");
    loaded.get_global("result").unwrap();
    assert_eq!(loaded.to_bytes(-1).unwrap(), [255]);
    loaded.pop(1).unwrap();
}

#[test]
fn uncached_set_field_bytecode_loads_from_a_save() {
    let assignments = "t.x = 1\n".repeat(256);
    let mut original = fresh();
    run(
        &mut original,
        &format!("function f() local t = {{}}\n{assignments}return t.x end"),
    );

    let mut loaded = reload(&original);
    run(&mut loaded, "result = f()");
    loaded.get_global("result").unwrap();
    assert_eq!(loaded.to_number(-1).expect("result should be numeric"), 1.0);
    loaded.pop(1).unwrap();
}

#[test]
fn compiler_produced_save_resaves_byte_for_byte() {
    let mut original = fresh();
    run(
        &mut original,
        r#"
        function outer(a)
            return function(b) return a + b end
        end
        f = outer(40)
        t = { answer = f(2), binary = "\255" }
    "#,
    );
    let first = original.save_state().unwrap().bytes;
    let loaded = State::load_state(&first, Box::new(DefaultCallbacks), |_| {}).unwrap();
    assert_eq!(loaded.save_state().unwrap().bytes, first);
}

#[test]
fn non_utf8_and_edge_values_round_trip() {
    let mut state = fresh();

    state
        .push_bytes([0u8, 159, 146, 150, 255])
        .expect("short test string fits");
    state.set_global("binval");
    state.push_number(-0.0).unwrap();
    state.set_global("negzero");
    state.push_number(f64::NAN).unwrap();
    state.set_global("nan");

    // Table keyed by a non-UTF8 byte string.
    state.new_table().unwrap();
    state
        .push_bytes([0xff, 0x00, 0xfe])
        .expect("short test string fits");
    state.push_number(42.0).unwrap();
    state.set_table_raw(-3).unwrap();
    state.set_global("bt");

    let mut loaded = reload(&state);

    loaded.get_global("binval").unwrap();
    assert_eq!(loaded.to_bytes(-1).unwrap(), [0u8, 159, 146, 150, 255]);
    loaded.pop(1).unwrap();

    loaded.get_global("negzero").unwrap();
    assert_eq!(loaded.to_number(-1).unwrap().to_bits(), (-0.0f64).to_bits());
    loaded.pop(1).unwrap();

    loaded.get_global("nan").unwrap();
    assert!(loaded.to_number(-1).unwrap().is_nan());
    loaded.pop(1).unwrap();

    loaded.get_global("bt").unwrap();
    loaded
        .push_bytes([0xff, 0x00, 0xfe])
        .expect("short test string fits");
    loaded.get_table_raw(-2).unwrap();
    assert_eq!(loaded.to_number(-1).unwrap(), 42.0);
}

#[test]
fn large_table_pairs_order_round_trips() {
    let mut original = fresh();
    run(
        &mut original,
        r#"
        order = {}
        for i = 1, 30 do order["k" .. i] = i end
        keys = ""
        for k in pairs(order) do keys = keys .. k .. "," end
    "#,
    );
    let before = global_str(&mut original, "keys");

    let mut loaded = reload(&original);
    run(
        &mut loaded,
        "keys2 = \"\"\nfor k in pairs(order) do keys2 = keys2 .. k .. \",\" end",
    );
    assert_eq!(global_str(&mut loaded, "keys2"), before);
}

#[test]
fn corruption_sweep_never_panics() {
    let mut state = fresh();
    run(
        &mut state,
        r#"
        t = { a = 1, b = "two", c = { 3, 4, 5 } }
        t.self = t
        f = string.gsub
        counter = (function() local n = 1 return function() n = n + 1 return n end end)()
    "#,
    );
    let bytes = state.save_state().unwrap().bytes;

    // Consume the result without inspecting it: we only require that no input
    // panics or OOMs. A panic during load aborts the test.
    fn ignore(_: Result<State, LoadError>) {}

    // Truncating at every prefix length must error, never panic or OOM.
    for len in 0..bytes.len() {
        ignore(State::load_state(
            &bytes[..len],
            Box::new(DefaultCallbacks),
            |_| {},
        ));
    }
    // A single-byte flip at every offset must load-or-error, never panic.
    for i in 0..bytes.len() {
        let mut corrupted = bytes.clone();
        corrupted[i] ^= 0xff;
        ignore(State::load_state(
            &corrupted,
            Box::new(DefaultCallbacks),
            |_| {},
        ));
    }
}

#[test]
fn same_bytecode_closures_and_binary_literals_survive_snapshot() {
    // Two closures of one factory Bytecode share a rebuilt runtime bundle on
    // load (internal sharing is pinned by a vm.rs unit test; this asserts the
    // observable half), and a non-UTF-8 string literal must round-trip
    // through the load-time interning path.
    let mut original = fresh();
    run(
        &mut original,
        r#"
            local key = "\255\254bin"
            function make_adder(n)
                return function(t) return t[key] + n end
            end
            add1 = make_adder(1)
            add2 = make_adder(2)
            subject = { ["\255\254bin"] = 40 }
        "#,
    );
    let mut loaded = reload(&original);
    run(
        &mut loaded,
        "r1 = add1(subject) r2 = add2(subject) r3 = add1(subject)",
    );
    assert_eq!(global_num(&mut loaded, "r1"), 41.0);
    assert_eq!(global_num(&mut loaded, "r2"), 42.0);
    assert_eq!(global_num(&mut loaded, "r3"), 41.0);
}

#[test]
fn table_library_extension_survives_snapshot() {
    let mut original = fresh();
    run(&mut original, "table.custom = function(t) return 17 end");
    let mut loaded = reload(&original);
    run(&mut loaded, "result = ({}):custom()");
    assert_eq!(global_num(&mut loaded, "result"), 17.0);
}

#[test]
fn rebound_table_library_survives_snapshot() {
    let mut original = fresh();
    run(
        &mut original,
        "table = { custom = function(t) return 23 end }",
    );
    let mut loaded = reload(&original);
    run(&mut loaded, "result = ({}):custom()");
    assert_eq!(global_num(&mut loaded, "result"), 23.0);
}

#[test]
fn table_library_metatable_fallback_survives_snapshot() {
    let mut original = fresh();
    run(
        &mut original,
        r#"
            setmetatable(table, { __index = function(_, key)
                if key == "custom" then return function() return 29 end end
            end })
        "#,
    );
    let mut loaded = reload(&original);
    run(&mut loaded, "result = ({}):custom()");
    assert_eq!(global_num(&mut loaded, "result"), 29.0);
}

#[test]
fn ordered_table_library_replay_drops_the_pristine_fallback_cache() {
    // The final library must have exactly SEVEN live keys - the pristine
    // slot count - with different membership, and a forced reorder so the
    // load path takes the ordered `clear_and_insert_entries` replay. An
    // eight-key table would fail the shape guard on slot count alone and
    // never exercise the explicit cache drop this test exists to pin:
    // without the drop, a seven-key rebuild can coincidentally restore
    // pristine-looking shape fields while `custom` has replaced `insert`,
    // and the gate would answer nil for a key the table actually has.
    let mut original = fresh();
    run(
        &mut original,
        r#"
            table.insert = nil
            table.custom = function() return 31 end
            local remove = table.remove
            table.remove = nil
            table.remove = remove
        "#,
    );
    let mut loaded = reload(&original);
    run(
        &mut loaded,
        "result = ({}):custom() \
         insert_gone = (({}).insert == nil) and 1 or 0",
    );
    assert_eq!(global_num(&mut loaded, "result"), 31.0);
    assert_eq!(global_num(&mut loaded, "insert_gone"), 1.0);
}
