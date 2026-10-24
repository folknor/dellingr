use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dellingr::{
    ArgCount, DefaultCallbacks, HostCallbacks, LoadError, LuaType, RetCount, SaveError,
    SnapshotError, State,
};

use crate::support;

#[derive(Clone, Default)]
struct Capture {
    lines: Arc<Mutex<Vec<String>>>,
}

impl HostCallbacks for Capture {
    fn on_print(&mut self, _source: Option<&[u8]>, _line: u32, message: &[u8]) {
        self.lines
            .lock()
            .unwrap()
            .push(String::from_utf8(message.to_vec()).expect("test prints are UTF-8"));
    }
}

#[test]
fn deep_global_chain_saves_and_loads_iteratively() {
    let mut state = State::new();
    state.gc_disable_auto();
    support::run(
        &mut state,
        "deep = {}\nfor i = 1, 50000 do deep = { next = deep } end",
    );
    let save = state.save_state().expect("deep chain saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup)
        .expect("deep chain loads");
    support::run(
        &mut loaded,
        "local t = deep\nfor i = 1, 50000 do t = t.next end\ndone = t ~= nil",
    );
}

/// Run `source` as chunk `t`, calling global `go`, and return the rendered
/// traceback frames of the error.
fn traceback_frames(state: &mut State, source: &str) -> Vec<String> {
    dellingr::Program::compile(source, Some(b"t".as_slice()))
        .and_then(|program| state.load(&program))
        .unwrap();
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("program must fail");
    error
        .stack_trace()
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn traceback_labels_are_identical_before_and_after_save_load() {
    let mut original = State::new();
    dellingr::Program::compile(
        "anon = function()\n  error('x')\nend\n\nfunction named()\n  anon()\nend\n",
        Some(b"t".as_slice()),
    )
    .and_then(|program| original.load(&program))
    .unwrap();
    original
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .unwrap();

    let call = "named()";
    let before = traceback_frames(&mut original, call);
    assert_eq!(
        before,
        [
            "t:2: in function <t:1>",
            "t:6: in function 'named'",
            "t:1: in main chunk",
        ]
    );

    let mut loaded = support::reload(&original);
    let after = traceback_frames(&mut loaded, call);
    assert_eq!(before, after);
}

#[test]
fn global_environment_preserves_non_string_key_identity_across_save_load() {
    let mut state = State::new();
    support::run(&mut state, "_G[1] = 'number'; _G['1'] = 'string'");
    let save = state.save_state().expect("state saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup)
        .expect("state loads");
    support::eval(&mut loaded, "return _G[1], _G['1']", 2);
    assert_eq!(loaded.to_string(-2).unwrap(), "number");
    assert_eq!(loaded.to_string(-1).unwrap(), "string");
}

#[test]
fn global_environment_preserves_non_utf8_keys_across_save_load() {
    let mut state = State::new();
    support::run(
        &mut state,
        r#"_G["\xff"] = { value = "\xfe" }; _G["\xfe"] = "other";
            _G["\xef\xbf\xbd"] = "utf8"; _G["\xff"].env = _G"#,
    );
    state.gc_collect();
    let save = state.save_state().expect("byte-keyed proxy saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup)
        .expect("proxy loads");
    loaded.gc_collect();
    assert_eq!(loaded.save_state().unwrap().bytes, save.bytes);
    support::run(
        &mut loaded,
        r#"if _G["\xff"].value ~= "\xfe" then error("byte value lost") end
            if _G["\xff"].env ~= _G then error("proxy cycle lost") end
            if _G["\xfe"] ~= "other" then error("byte key aliased") end
            if _G["\xef\xbf\xbd"] ~= "utf8" then error("UTF-8 key aliased") end
            if getmetatable(_G) ~= false then error("protection lost") end
            _G["\xff"] = nil
            if _G["\xff"] ~= nil then error("byte key not cleared") end
            if _G["\xfe"] ~= "other" then error("other key changed") end"#,
    );
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
    support::run(&mut control, setup);
    support::run(&mut control, continuation);

    let (mut original, _) = new_capture_state();
    support::run(&mut original, setup);
    let save = original.save_state().unwrap();
    assert!(!save.bytes.is_empty());

    let (loaded_callbacks, loaded_lines) = {
        let callbacks = Capture::default();
        let lines = Arc::clone(&callbacks.lines);
        (callbacks, lines)
    };
    let mut loaded =
        State::load_state(&save.bytes, Box::new(loaded_callbacks), support::no_setup).unwrap();
    support::run(&mut loaded, continuation);

    assert_eq!(
        *loaded_lines.lock().unwrap(),
        *control_lines.lock().unwrap()
    );
}

#[test]
fn tombstones_are_preserved_in_snapshots() {
    // Both inline and map tombstones, including a vacated map position,
    // are saved even though pairs skips them.
    for (tombstoned, clean, expected_order, dead_b_successor) in [
        (
            "t = {}; t.a = 1; t.b = 2; t.c = 3; t.b = nil",
            "t = {}; t.a = 1; t.c = 3",
            "acb",
            Some("c"),
        ),
        (
            "t = {}; t.a = 1; t.b = 2; t.c = 3; t.d = 4; t.e = 5; t.f = 6; t.b = nil",
            "t = {}; t.a = 1; t.c = 3; t.d = 4; t.e = 5; t.f = 6",
            "acdefb",
            Some("c"),
        ),
        (
            "t = {}; t.a = 1; t.b = 2; t.c = 3; t.d = 4; t.e = 5; t.f = 6; t.b = nil; t.b = 2",
            "t = {}; t.a = 1; t.c = 3; t.d = 4; t.e = 5; t.f = 6; t.b = 2",
            "acdefb",
            None,
        ),
    ] {
        let (mut with_tombstone, _) = new_capture_state();
        support::run(&mut with_tombstone, tombstoned);
        let tombstoned_bytes = with_tombstone.save_state().unwrap().bytes;

        let (mut without_tombstone, _) = new_capture_state();
        support::run(&mut without_tombstone, clean);
        let clean_bytes = without_tombstone.save_state().unwrap().bytes;

        if let Some(successor) = dead_b_successor {
            let mut restored_tombstone = State::load_state(
                &tombstoned_bytes,
                Box::new(DefaultCallbacks),
                support::no_setup,
            )
            .unwrap();
            support::eval(&mut restored_tombstone, "return next(t, 'b')", 1);
            assert_eq!(restored_tombstone.to_string(-1).unwrap(), successor);
            restored_tombstone.pop(1).unwrap();

            let mut restored_clean =
                State::load_state(&clean_bytes, Box::new(DefaultCallbacks), support::no_setup)
                    .unwrap();
            let error = support::try_run(&mut restored_clean, "next(t, 'b')")
                .expect_err("a save without the dead slot rejects it as a cursor");
            assert!(error.to_string().contains("invalid key to 'next'"));
        }

        // And the live entries still round-trip with insertion order intact,
        // with a reinserted key landing at the back.
        let mut loaded = State::load_state(
            &tombstoned_bytes,
            Box::new(DefaultCallbacks),
            support::no_setup,
        )
        .unwrap();
        support::run(
            &mut loaded,
            "t.b = 4; order = ''; for k in pairs(t) do order = order .. k end",
        );
        loaded.get_global("order").unwrap();
        assert_eq!(loaded.to_string(-1).unwrap(), expected_order);
        loaded.pop(1).unwrap();
    }
}

#[test]
fn cleared_cursor_resumes_across_collection_and_snapshot() {
    for setup in [
        "work = {}; for i = 1, 3 do work['job' .. i] = i end",
        "work = {}; for i = 1, 8 do work['job' .. i] = i end",
        "work = {a = nil, b = 1, c = 2, d = 3, e = 4}; cursor = 'a'",
        "work = {}; for i = 1, 8 do work[{}] = i end",
        "work = math; work.job1 = 1; work.job2 = 2",
    ] {
        let mut original = State::new();
        support::run(&mut original, setup);
        support::run(
            &mut original,
            "function tick() local k = next(work, cursor); if k == nil then cursor = nil; return end; work[k] = nil; cursor = k end; tick()",
        );
        original.gc_collect();
        let save = original.save_state().unwrap();
        let mut loaded =
            State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup).unwrap();
        assert_eq!(save.bytes, loaded.save_state().unwrap().bytes);
        for _ in 0..12 {
            support::run(&mut original, "tick()");
            support::run(&mut loaded, "tick()");
            original.gc_collect();
            loaded.gc_collect();
            assert_eq!(
                original.save_state().unwrap().bytes,
                loaded.save_state().unwrap().bytes,
                "cursor state diverged for {setup}"
            );
        }
    }
}

#[test]
fn nil_template_control_survives_snapshot() {
    let mut original = State::new();
    // More than four named fields make the constructor a template, which
    // pre-inserts every key as a dead slot.
    support::run(
        &mut original,
        "work = {a = nil, b = 1, c = 2, d = 3, e = 4}",
    );
    let mut loaded = support::reload(&original);
    support::run(
        &mut original,
        "if next(work, 'a') ~= 'b' then error('lost template cursor') end",
    );
    support::run(
        &mut loaded,
        "if next(work, 'a') ~= 'b' then error('lost template cursor') end",
    );
    support::run(&mut original, "work.a = 2; work.b = nil; work.c = 3");
    support::run(&mut loaded, "work.a = 2; work.b = nil; work.c = 3");
    assert_eq!(
        original.save_state().unwrap().bytes,
        loaded.save_state().unwrap().bytes
    );
}

#[test]
fn tombstone_is_the_only_root_of_its_collectable_key() {
    let mut original = State::new();
    support::run(
        &mut original,
        "work = {}; local key = {}; work[key] = 1; work[key] = nil",
    );
    let before = original.save_state().unwrap().bytes;
    original.gc_collect();
    assert_eq!(before, original.save_state().unwrap().bytes);
    let mut loaded =
        State::load_state(&before, Box::new(DefaultCallbacks), support::no_setup).unwrap();
    loaded.gc_collect();
    assert_eq!(before, loaded.save_state().unwrap().bytes);
}

fn byte_stability_state(with_slotmap_history: bool) -> State {
    let (mut state, _) = new_capture_state();
    state.gc_disable_auto();

    if with_slotmap_history {
        support::run(
            &mut state,
            r#"
            for i = 1, 1000 do
                local throwaway_table = {}
                local throwaway_string = "slot history " .. i
                local throwaway_closure = function() return throwaway_string end
            end
            "#,
        );
        state.gc_collect();
    }

    support::run(
        &mut state,
        r#"
        t = { a = 1, b = "x" }
        t.self = t
        local captured = "closed upvalue"
        f = function(value) return captured .. value end
        g = string.gsub
        identity_table = {}
        identity_string = "rendered string"
        identity_closure = function() return captured end
        table_id = string.format("%p", identity_table)
        string_id = string.format("%p", identity_string)
        closure_id = string.format("%p", identity_closure)
    "#,
    );
    state.gc_collect();
    state.set_cost_budget(i64::MAX);
    state
}

#[test]
fn saves_are_byte_stable() {
    let with_slotmap_history = byte_stability_state(true);
    let direct = byte_stability_state(false);
    assert_eq!(
        with_slotmap_history.save_state().unwrap().bytes,
        direct.save_state().unwrap().bytes
    );
}

#[test]
fn state_is_quiescent_after_error_in_dynamic_call_args() {
    // An error while evaluating a dynamic call's arguments (after the base was
    // marked, before the call ran) must not leave a stale pending call base,
    // or the State would fail quiescence validation and could not be
    // snapshotted.
    let mut state = State::new();
    support::load(
        &mut state,
        r#"
            local function f(...) return ... end
            local function g() return nil + 1 end
            return f(g())
        "#,
    );
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
    support::load(
        &mut state,
        r#"
            local function f(...) return nil + 1 end
            return f({ marker = 42 })
        "#,
    );
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
    support::load(
        &mut state,
        r#"
            local t = { { v = 2 }, { v = 1 } }
            table.sort(t, function() return nil + 1 end)
        "#,
    );
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
        dellingr::Program::compile("return nil + 1", None)
            .and_then(|program| state.load(&program))?;
        state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
    });
    result
        .expect("state is not poisoned")
        .expect_err("restricted callback must error");
    state
        .save_state()
        .expect("suspended environment must be restored after an error");
}

#[test]
fn save_state_rejects_suspended_environment() {
    let mut state = State::new();
    state
        .with_restricted_env(&[], |state| {
            assert!(matches!(state.save_state(), Err(SaveError::NotQuiescent)));
        })
        .expect("state is not poisoned");
}

#[test]
fn state_is_quiescent_after_error_in_table_constructor() {
    // Same guard for pending constructor bases: an error between
    // NewTableTracked and its SetList must not leak a base.
    let mut state = State::new();
    support::load(
        &mut state,
        r#"
            local function g() return nil + 1 end
            return {g()}
        "#,
    );
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
    support::eval(
        &mut state,
        r#"
            f = string.gmatch("a b c", "%w+")
            return f()
        "#,
        1,
    );
    assert_eq!(state.to_string(-1).unwrap(), "a");
    state.pop(1).unwrap();

    let save = state.save_state().unwrap();
    let mut loaded =
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup).unwrap();

    loaded.get_global("f").unwrap();
    loaded.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(loaded.to_string(-1).unwrap(), "b");
}

#[test]
fn gmatch_last_match_end_survives_save_load() {
    // The saved iterator must remember the previous end, or resuming after
    // "abc" would incorrectly accept a trailing empty match.
    let mut state = State::new();
    support::eval(
        &mut state,
        r#"f = string.gmatch("abc", "%a*"); return f()"#,
        1,
    );
    assert_eq!(state.to_string(-1).unwrap(), "abc");
    state.pop(1).unwrap();

    let save = state.save_state().unwrap();
    let mut loaded =
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup).unwrap();
    loaded.get_global("f").unwrap();
    loaded.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(loaded.typ(-1), LuaType::Nil);
}

#[test]
fn empty_gmatch_closure_survives_save_load() {
    // The empty-pattern gmatch path uses a different iterator id; it too must be
    // registered at library-open so its closure restores.
    let mut state = State::new();
    support::run(&mut state, r#"g = string.gmatch("ab", "")"#);

    let save = state.save_state().unwrap();
    let mut loaded =
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup).unwrap();

    loaded.get_global("g").unwrap();
    loaded.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(loaded.to_string(-1).unwrap(), "");
}

#[test]
fn restored_gmatch_iterator_errors_point_at_the_script_caller() {
    // The unfinished capture fails lazily, on the first call after the load.
    // The restored iterator must be recognised as the gmatch wrapper, so the
    // error lands on the calling script line, not in the synthetic chunk.
    let mut state = State::new();
    support::run(&mut state, "bad = string.gmatch('a', '(')");
    let save = state.save_state().unwrap();
    for (source, line) in [("\n\nbad()", 3), ("\nfor value in bad do end", 2)] {
        let mut loaded =
            State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup).unwrap();
        dellingr::Program::compile(source, Some(b"after-load".as_slice()))
            .and_then(|program| loaded.load(&program))
            .unwrap();
        let error = loaded
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect_err("unfinished capture fails lazily");
        assert_eq!(error.source_name(), Some(b"after-load".as_slice()));
        assert_eq!(error.line_num(), line);
        assert_eq!(error.stack_trace()[0].line, line);
        assert!(
            error
                .stack_trace()
                .iter()
                .all(|frame| frame.source.as_deref() != Some(b"string.gmatch".as_slice())),
            "the wrapper frame stays hidden: {:?}",
            error.stack_trace()
        );
        assert!(
            error
                .to_string()
                .starts_with(&format!("after-load:{line}:"))
        );
    }
}

#[test]
fn empty_state_round_trip_stays_empty() {
    let state = State::raw();
    let save = state.save_state().unwrap();
    let mut loaded =
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup).unwrap();

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
    support::run(
        &mut state,
        "iter, iter_state, iter_key = pairs({ a = 7 })\nnext = 42",
    );
    let save = state.save_state().unwrap();
    let mut loaded =
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup).unwrap();
    support::run(
        &mut loaded,
        "for key, value in iter, iter_state, iter_key do result = value end",
    );
    loaded.get_global("result").unwrap();
    assert_eq!(loaded.to_number(-1).unwrap(), 7.0);
}

#[test]
fn dynamic_table_constructor_round_trips_after_completion() {
    let (mut original, _) = new_capture_state();
    support::run(
        &mut original,
        r#"
        function values()
            return 2, 3, 5
        end
        t = {1, values()}
    "#,
    );
    let save = original.save_state().unwrap();
    let mut loaded =
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup).unwrap();
    support::run(&mut loaded, "a = #t; b = t[1] + t[2] + t[3] + t[4]");
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
    support::run(&mut control, "math.random(); math.random()");
    support::run(
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
    support::run(&mut original, "math.random(); math.random()");
    let save = original.save_state().unwrap();

    let callbacks = Capture::default();
    let loaded_lines = Arc::clone(&callbacks.lines);
    let mut loaded =
        State::load_state(&save.bytes, Box::new(callbacks), support::no_setup).unwrap();
    assert_eq!(loaded.cost_used(), original.cost_used());
    assert_eq!(loaded.cost_remaining(), original.cost_remaining());
    support::run(
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
    support::run(&mut original, "t = {1, 2, 3, 4, 5}");
    original.set_cost_budget(2);
    let save = original.save_state().expect("state saves");

    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup)
        .expect("state loads");
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
        error.kind(),
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
    state.set_global("host_fn").unwrap();

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

    let loaded =
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup).unwrap();
    assert_eq!(loaded.anchor_count(), 0);

    assert!(state.release_anchor(anchor));
}

#[test]
fn environment_deltas_preserve_mutations_cycles_order_and_metatables() {
    let mut original = State::new();
    support::run(
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
    let mut loaded = support::reload(&original);
    support::run(
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
    assert_eq!(support::global_number(&mut loaded, "floor_value"), 42.0);
    assert_eq!(support::global_number(&mut loaded, "deleted"), 1.0);
    assert_eq!(support::global_number(&mut loaded, "cycle"), 1.0);
    assert_eq!(support::global_number(&mut loaded, "meta"), 1.0);
    assert_eq!(
        support::global_string(&mut loaded, "order"),
        "second,first,"
    );
    // A loaded state remains quiescent and can be saved immediately.
    loaded.save_state().expect("loaded state resaves");
}

#[test]
fn pointer_identities_and_counter_survive_save_load() {
    let mut original = State::new();
    expose_global_env_metatable(&mut original);
    support::run(
        &mut original,
        "t = {}; s = 'reachable'; saved_identity = tostring(t); saved_percent_p = string.format('%p', t); saved_string_p = string.format('%p', s); saved_rust_fn_p = string.format('%p', print); saved_env_child_p = string.format('%p', env_child); dead = {}; tostring(dead); dead = nil",
    );
    let mut loaded = support::reload(&original);
    // Re-fetch from the live `_G` so the comparison reads the rebuilt
    // environment child, not just the restored global.
    expose_global_env_metatable(&mut loaded);
    support::run(
        &mut loaded,
        "loaded_identity = tostring(t); loaded_percent_p = string.format('%p', t); loaded_string_p = string.format('%p', s); loaded_rust_fn_p = string.format('%p', print); loaded_env_child_p = string.format('%p', env_child); later = {}; later_identity = tostring(later)",
    );
    assert_eq!(
        support::global_string(&mut loaded, "loaded_identity"),
        support::global_string(&mut loaded, "saved_identity")
    );
    assert_eq!(
        support::global_string(&mut loaded, "loaded_percent_p"),
        support::global_string(&mut loaded, "saved_percent_p")
    );
    assert_eq!(
        support::global_string(&mut loaded, "loaded_string_p"),
        support::global_string(&mut loaded, "saved_string_p")
    );
    assert_eq!(
        support::global_string(&mut loaded, "loaded_rust_fn_p"),
        support::global_string(&mut loaded, "saved_rust_fn_p")
    );
    assert_eq!(
        support::global_string(&mut loaded, "loaded_env_child_p"),
        support::global_string(&mut loaded, "saved_env_child_p")
    );
    // The collected object's id is intentionally not reused.
    assert_eq!(
        support::global_string(&mut loaded, "later_identity"),
        "table: 0x6"
    );
}

#[test]
fn setup_rendered_identities_are_rekeyed_above_the_saved_counter() {
    let mut original = State::new();
    support::run(
        &mut original,
        "t = {}; saved_identity = tostring(t); dead = {}; tostring(dead); dead = nil",
    );
    let save = original.save_state().unwrap();
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        support::run(
            state,
            "setup_only = {}; setup_seen = tostring(setup_only); temp = {}; tostring(temp); temp = nil",
        );
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap();
    support::run(
        &mut loaded,
        "t_after = tostring(t); setup_after = tostring(setup_only); later = tostring({})",
    );
    // The saved value keeps its saved identity.
    assert_eq!(
        support::global_string(&mut loaded, "saved_identity"),
        "table: 0x1"
    );
    assert_eq!(support::global_string(&mut loaded, "t_after"), "table: 0x1");
    // This setup differs from the saving State's: it rendered its own table
    // as 0x1, which the save gives to `t`. The surviving table is re-keyed
    // above the saved counter (3), so it collides with neither `t` nor the
    // retired id 2. Setup's dead `temp` consumes no id.
    assert_eq!(
        support::global_string(&mut loaded, "setup_seen"),
        "table: 0x1"
    );
    assert_eq!(
        support::global_string(&mut loaded, "setup_after"),
        "table: 0x3"
    );
    assert_eq!(support::global_string(&mut loaded, "later"), "table: 0x4");
}

/// Initialization shared by a State and the setup that loads its save:
/// render a fresh table and keep it only through an anchor, which saves do
/// not record.
fn anchor_rendered_table(state: &mut State) -> (dellingr::Anchor, String) {
    state.new_table().unwrap();
    let rendered = state.to_string_with_meta(-1).unwrap();
    (state.anchor().unwrap(), rendered)
}

#[test]
fn replaying_setup_keeps_its_anchored_identities() {
    let mut original = State::new();
    let (original_anchor, original_rendered) = anchor_rendered_table(&mut original);
    support::run(&mut original, "t = {}; saved_identity = tostring(t)");
    let save = original.save_state().unwrap();
    let mut from_setup = None;
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        from_setup = Some(anchor_rendered_table(state));
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap();
    let (loaded_anchor, setup_rendered) = from_setup.expect("setup ran");
    assert_eq!(setup_rendered, original_rendered);
    assert_eq!(original_rendered, "table: 0x1");
    // The loaded State continues exactly as the continuous run: the anchored
    // table keeps the id setup gave it, and the next fresh id matches.
    let continue_run = |state: &mut State, anchor: &dellingr::Anchor| {
        state.push_anchor(anchor).unwrap();
        let anchored = state.to_string_with_meta(-1).unwrap();
        state.pop(1).unwrap();
        state.new_table().unwrap();
        let fresh_table = state.to_string_with_meta(-1).unwrap();
        state.pop(1).unwrap();
        (anchored, fresh_table)
    };
    assert_eq!(
        continue_run(&mut loaded, &loaded_anchor),
        continue_run(&mut original, &original_anchor)
    );
    assert_eq!(
        loaded.save_state().unwrap().bytes,
        original.save_state().unwrap().bytes
    );
}

#[test]
fn ordered_environment_replay_preserves_setup_additions_and_metatable() {
    let mut original = State::new();
    // Deleting and re-adding a STOCK key is what forces an explicit order
    // vector: `floor` moves to the end of the live table, whereas plain
    // delete/upsert replay would restore it in its original baseline position.
    // Adding only new keys replays in order naturally and emits no vector, so
    // it would not exercise this path at all.
    support::run(&mut original, "math.floor = nil; math.floor = 7");
    let save = original.save_state().expect("state saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        support::run(
            state,
            "math.future = 99; local mt = { setup = true }; setmetatable(math, mt)",
        );
        Ok::<(), std::convert::Infallible>(())
    })
    .expect("state loads with later environment additions");
    support::run(
        &mut loaded,
        "floor = math.floor; future = math.future; setup_meta = getmetatable(math).setup and 1 or 0; order = ''; for key in pairs(math) do if key == 'floor' or key == 'future' then order = order .. key .. ',' end end",
    );
    assert_eq!(support::global_number(&mut loaded, "floor"), 7.0);
    // A key the save never saw survives replay instead of failing the load.
    assert_eq!(support::global_number(&mut loaded, "future"), 99.0);
    // Rebuilding the entry list must not drop a metatable installed by setup.
    assert_eq!(support::global_number(&mut loaded, "setup_meta"), 1.0);
    // Saved keys keep their saved order; setup-only keys are appended after.
    assert_eq!(
        support::global_string(&mut loaded, "order"),
        "floor,future,"
    );
}

#[test]
fn ordered_environment_replay_invalidates_setup_field_caches() {
    let mut original = State::new();
    // Moving a stock key to the end forces ordered replay. Other stock keys
    // then move to different indices, even though their values stay unchanged.
    support::run(&mut original, "math.floor = nil; math.floor = 7");
    let save = original.save_state().expect("state saves");
    let mut probe = None;
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        // Capture the canonical table in a setup closure, warm its field cache
        // at version 0 and retain the same closure across materialization.
        dellingr::Program::compile("local m = math return function() return m.floor end", None)
            .and_then(|program| state.load(&program))?;
        state.call(ArgCount::Fixed(0), RetCount::Fixed(1))?;
        let anchor = state.anchor_function()?;
        state.call_anchor(&anchor, ArgCount::Fixed(0), RetCount::Fixed(1))?;
        assert_eq!(state.typ(-1), LuaType::Function);
        state.pop(1)?;
        probe = Some(anchor);
        Ok::<(), dellingr::error::Error>(())
    })
    .expect("state loads after warming a cache in setup");
    loaded
        .call_anchor(
            probe.as_ref().expect("setup retained the probe"),
            ArgCount::Fixed(0),
            RetCount::Fixed(1),
        )
        .expect("retained probe runs after ordered replay");
    assert_eq!(loaded.to_number(-1).expect("saved floor is numeric"), 7.0);
    loaded.pop(1).unwrap();
}

#[test]
fn unordered_environment_replay_places_setup_additions_before_new_keys() {
    // The counterpart to the test above: when replay reproduces the saved order
    // naturally, no order vector is emitted and a setup-added key simply sits
    // where it was inserted - ahead of keys the delta appends. Lua does not
    // specify `pairs` order, so what is pinned here is only that saved keys keep
    // their relative order and the setup addition survives.
    let mut original = State::new();
    support::run(
        &mut original,
        "math.first = 1; math.second = 2; math.first = nil; math.first = 3",
    );
    let save = original.save_state().expect("state saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        support::run(state, "math.future = 99");
        Ok::<(), std::convert::Infallible>(())
    })
    .expect("state loads");
    support::run(
        &mut loaded,
        "future = math.future; order = ''; for key in pairs(math) do if key == 'second' or key == 'first' or key == 'future' then order = order .. key .. ',' end end",
    );
    assert_eq!(support::global_number(&mut loaded, "future"), 99.0);
    assert_eq!(
        support::global_string(&mut loaded, "order"),
        "second,first,future,"
    );
}

#[test]
fn setup_anchor_survives_load_final_gc() {
    let original = State::new();
    let save = original.save_state().expect("state saves");
    let mut setup_anchor = None;
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        state.push_number(42.0)?;
        setup_anchor = Some(state.anchor()?);
        Ok::<(), dellingr::error::Error>(())
    })
    .expect("state loads");
    loaded
        .push_anchor(setup_anchor.as_ref().expect("setup produced anchor"))
        .unwrap();
    assert_eq!(loaded.to_number(-1).unwrap(), 42.0);
    loaded.pop(1).unwrap();
}

#[test]
fn unsupported_format_versions_fail_before_setup() {
    let state = State::new();
    let save = state.save_state().expect("state saves");
    // The header's format version follows the "DLGS" magic.
    let current = u16::from_le_bytes([save.bytes[4], save.bytes[5]]);
    for version in [current - 1, current + 1] {
        let mut bytes = save.bytes.clone();
        bytes[4..6].copy_from_slice(&version.to_le_bytes());
        let setup_ran = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&setup_ran);
        assert!(matches!(
            State::load_state(&bytes, Box::new(DefaultCallbacks), move |_| {
                observed.store(true, Ordering::SeqCst);
                Ok::<(), std::convert::Infallible>(())
            }),
            Err(LoadError::Snapshot(SnapshotError::UnsupportedVersion { found, expected }))
                if found == version && expected == current
        ));
        assert!(!setup_ran.load(Ordering::SeqCst));
    }
}

#[test]
fn unsupported_cost_model_versions_fail_before_setup() {
    let state = State::new();
    let save = state.save_state().expect("state saves");
    let current = dellingr::COST_MODEL_VERSION;
    for version in [1_u16, current - 1, current + 1] {
        let mut bytes = save.bytes.clone();
        bytes[6..8].copy_from_slice(&version.to_le_bytes());
        let setup_ran = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&setup_ran);
        assert!(matches!(
            State::load_state(&bytes, Box::new(DefaultCallbacks), move |_| {
                observed.store(true, Ordering::SeqCst);
                Ok::<(), std::convert::Infallible>(())
            }),
            Err(LoadError::Snapshot(SnapshotError::UnsupportedCostModelVersion { found, expected }))
                if found == version && expected == dellingr::COST_MODEL_VERSION
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
    support::run(&mut state, &source);
    let saved = state.save_state().expect("wide bytecode should save");
    let mut loaded = State::load_state(&saved.bytes, Box::new(DefaultCallbacks), support::no_setup)
        .expect("wide bytecode should load");
    support::eval(&mut loaded, "return answer", 1);
    assert_eq!(
        loaded.to_number(-1).expect("answer should be numeric"),
        510.0
    );
}

#[test]
fn non_quiescent_save_errors() {
    let mut state = State::new();
    support::load(&mut state, "x = 1");

    assert_eq!(state.save_state().unwrap_err(), SaveError::NotQuiescent);
}

#[test]
fn bad_or_truncated_bytes_error_without_panicking() {
    let bad_magic = match State::load_state(b"nope", Box::new(DefaultCallbacks), support::no_setup)
    {
        Ok(_) => panic!("bad magic load should fail"),
        Err(err) => err,
    };
    assert_eq!(bad_magic, LoadError::Snapshot(SnapshotError::BadMagic));

    let state = State::new();
    let mut bytes = state.save_state().unwrap().bytes;
    bytes.pop();
    let truncated = match State::load_state(&bytes, Box::new(DefaultCallbacks), support::no_setup) {
        Ok(_) => panic!("truncated load should fail"),
        Err(err) => err,
    };
    assert!(matches!(
        truncated,
        LoadError::Snapshot(SnapshotError::DecodeError {
            reason: dellingr::snapshot::DecodeReason::TruncatedPayload,
            ..
        })
    ));
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

    let err = match State::load_state(&save.bytes, Box::new(DefaultCallbacks), support::no_setup) {
        Ok(_) => panic!("missing host function load should fail"),
        Err(err) => err,
    };
    assert_eq!(
        err,
        LoadError::Snapshot(SnapshotError::UnknownFunction("game.host_fn".to_string()))
    );
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
    support::run(&mut state, "saved_host_value = host_value");
    let save = state.save_state().unwrap();

    let callbacks = Capture::default();
    let lines = Arc::clone(&callbacks.lines);
    let mut loaded = State::load_state(&save.bytes, Box::new(callbacks), |state| {
        state.set_global_named_rust_fn("host_value", "game.host_value", host_value)
    })
    .unwrap();
    support::run(&mut loaded, "print(saved_host_value())");

    assert_eq!(*lines.lock().unwrap(), vec!["42".to_string()]);
}

#[test]
fn load_setup_registration_errors_are_returned_directly() {
    fn original_fn(state: &mut State) -> dellingr::Result<u8> {
        state.push_number(1.0)?;
        Ok(1)
    }
    fn conflicting_fn(state: &mut State) -> dellingr::Result<u8> {
        state.push_number(2.0)?;
        Ok(1)
    }

    let mut state = State::new();
    state
        .set_global_named_rust_fn("host_fn", "game.host_fn", original_fn)
        .unwrap();
    support::run(&mut state, "saved_host_fn = host_fn");
    let save = state.save_state().unwrap();

    let error = match State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        state.register_rust_fn("game.host_fn", original_fn)?;
        state.register_rust_fn("game.host_fn", conflicting_fn)
    }) {
        Ok(_) => panic!("conflicting setup registration must fail"),
        Err(error) => error,
    };
    assert_eq!(
        error,
        LoadError::Setup(SaveError::DuplicateFunctionRegistration {
            id: "game.host_fn".to_string(),
        })
    );
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
    first.set_global("host_fn").unwrap();

    let mut second = State::new();
    second.register_rust_fn("game.a", host_fn).unwrap();
    second.register_rust_fn("game.z", host_fn).unwrap();
    second.push_rust_fn(host_fn).unwrap();
    second.set_global("host_fn").unwrap();

    assert_eq!(
        first.save_state().unwrap().bytes,
        second.save_state().unwrap().bytes
    );
}

// ---------------------------------------------------------------------------
// Helpers for reading back globals after a load.
// ---------------------------------------------------------------------------

/// `_G`'s metatable is protected from scripts (`getmetatable(_G)` is
/// `false`), so the host API binds it to the global `env_child` for tests
/// that need a script-visible environment child.
fn expose_global_env_metatable(state: &mut State) {
    state.get_global("_G").expect("push _G");
    state.get_metatable_of(-1).expect("push _G metatable");
    state.set_global("env_child").expect("bind env_child");
    state.pop(1).expect("pop _G");
}

#[test]
fn shadowed_builtins_survive_round_trip() {
    let mut original = State::new();
    // Reassign a builtin function and a whole library table; both live in
    // `globals` under a builtin name but no longer hold the canonical value.
    support::run(&mut original, "print = 123\nmath = { tag = 7 }");

    let mut loaded = support::reload(&original);
    support::run(
        &mut loaded,
        "observed_print = type(print)\nobserved_math = math.tag",
    );
    assert_eq!(
        support::global_string(&mut loaded, "observed_print"),
        "number"
    );
    assert_eq!(support::global_number(&mut loaded, "observed_math"), 7.0);
}

#[test]
fn captured_env_ref_survives_builtin_shadow() {
    let mut original = State::new();
    support::run(
        &mut original,
        r#"
        m = math            -- capture the real library before shadowing
        math = { tag = 7 }  -- then shadow the global name
        before = m.floor(3.9)
    "#,
    );
    assert_eq!(support::global_number(&mut original, "before"), 3.0);

    let mut loaded = support::reload(&original);
    support::run(
        &mut loaded,
        "via_captured = m.floor(9.9)\nvia_shadow = math.tag",
    );
    // The captured reference resolves to the rebuilt real library; the shadowed
    // global resolves to the user's table. The env snapshot keeps them distinct.
    assert_eq!(support::global_number(&mut loaded, "via_captured"), 9.0);
    assert_eq!(support::global_number(&mut loaded, "via_shadow"), 7.0);
}

#[test]
fn env_object_as_metatable_resolves_after_load() {
    let mut original = State::new();
    support::run(
        &mut original,
        "t = {}\nsetmetatable(t, math)\nsame = getmetatable(t) == math and 1 or 0",
    );
    assert_eq!(support::global_number(&mut original, "same"), 1.0);

    let mut loaded = support::reload(&original);
    support::run(&mut loaded, "after = getmetatable(t) == math and 1 or 0");
    // The metatable resolves to the *rebuilt* env object, identical to `math`.
    assert_eq!(support::global_number(&mut loaded, "after"), 1.0);
}

#[test]
fn cyclic_metatable_round_trips() {
    let mut original = State::new();
    support::run(
        &mut original,
        "t = {}\nt.x = 42\nsetmetatable(t, t)\nt.__index = t",
    );

    let mut loaded = support::reload(&original);
    support::run(
        &mut loaded,
        "self_mt = getmetatable(t) == t and 1 or 0\ndirect = t.x",
    );
    assert_eq!(support::global_number(&mut loaded, "self_mt"), 1.0);
    assert_eq!(support::global_number(&mut loaded, "direct"), 42.0);
}

#[test]
fn closure_capturing_two_tables_round_trips() {
    // The snapshot verifier used to reject an ordinary Lua closure whose
    // upvalues are two distinct tables.
    let mut original = State::new();
    support::run(
        &mut original,
        r#"
        function make()
            local a = { n = 1 }
            local b = { n = 10 }
            return function()
                a.n = a.n + 1
                b.n = b.n + 10
                return a.n + b.n
            end
        end
        f = make()
        first = f()
    "#,
    );
    assert_eq!(support::global_number(&mut original, "first"), 22.0);

    let mut loaded = support::reload(&original);
    support::run(&mut loaded, "second = f()");
    assert_eq!(support::global_number(&mut loaded, "second"), 33.0);
}

#[test]
fn shared_upvalue_mutation_visible_across_closures() {
    let mut original = State::new();
    support::run(
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

    let mut loaded = support::reload(&original);
    support::run(&mut loaded, "before = getv()\nsetv(99)\nafter = getv()");
    // The carried-over state is observed, and the two closures still share one
    // upvalue cell: mutating through setv is visible through getv.
    assert_eq!(support::global_number(&mut loaded, "before"), 5.0);
    assert_eq!(support::global_number(&mut loaded, "after"), 99.0);
}

#[test]
fn deeply_nested_closures_round_trip() {
    let mut original = State::new();
    support::run(
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

    let mut loaded = support::reload(&original);
    support::run(&mut loaded, "r1 = f()\nr2 = g()");
    assert_eq!(support::global_number(&mut loaded, "r1"), 321.0);
    assert_eq!(support::global_number(&mut loaded, "r2"), 1002.0);
}

#[test]
fn saved_outer_creates_new_nested_closures() {
    let mut original = State::new();
    support::run(
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

    let mut loaded = support::reload(&original);
    support::run(&mut loaded, "result = outer(1)(20)(300)()");
    assert_eq!(support::global_number(&mut loaded, "result"), 321.0);
}

#[test]
fn upvalue_arena_entries_do_not_alias_during_save() {
    let mut original = State::new();
    support::run(
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

    let mut loaded = support::reload(&original);
    support::run(&mut loaded, "result = type(root())");
    assert_eq!(support::global_string(&mut loaded, "result"), "table");
}

#[test]
fn binary_string_literal_round_trips_through_save() {
    let mut original = State::new();
    support::run(
        &mut original,
        r#"
        function f()
            return "\255"
        end
    "#,
    );

    let mut loaded = support::reload(&original);
    support::run(&mut loaded, "result = f()");
    loaded.get_global("result").unwrap();
    assert_eq!(loaded.to_bytes(-1).unwrap(), [255]);
    loaded.pop(1).unwrap();
}

#[test]
fn uncached_set_field_bytecode_loads_from_a_save() {
    let assignments = "t.x = 1\n".repeat(256);
    let mut original = State::new();
    support::run(
        &mut original,
        format!("function f() local t = {{}}\n{assignments}return t.x end"),
    );

    let mut loaded = support::reload(&original);
    support::run(&mut loaded, "result = f()");
    loaded.get_global("result").unwrap();
    assert_eq!(loaded.to_number(-1).expect("result should be numeric"), 1.0);
    loaded.pop(1).unwrap();
}

#[test]
fn compiler_produced_save_resaves_byte_for_byte() {
    let mut original = State::new();
    support::run(
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
    let loaded = State::load_state(&first, Box::new(DefaultCallbacks), support::no_setup).unwrap();
    assert_eq!(loaded.save_state().unwrap().bytes, first);
}

#[test]
fn non_utf8_and_edge_values_round_trip() {
    let mut state = State::new();

    state
        .push_bytes([0u8, 159, 146, 150, 255])
        .expect("short test string fits");
    state.set_global("binval").unwrap();
    state.push_number(-0.0).unwrap();
    state.set_global("negzero").unwrap();
    state.push_number(f64::NAN).unwrap();
    state.set_global("nan").unwrap();

    // Table keyed by a non-UTF8 byte string.
    state.new_table().unwrap();
    state
        .push_bytes([0xff, 0x00, 0xfe])
        .expect("short test string fits");
    state.push_number(42.0).unwrap();
    state.set_table_raw(-3).unwrap();
    state.set_global("bt").unwrap();

    let mut loaded = support::reload(&state);

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
fn snapshot_preserves_first_inserted_negative_zero_table_key() {
    let mut original = State::new();
    support::run(
        &mut original,
        "t = {}; t[-0.0] = 1; t[0.0] = 2; for key in pairs(t) do zero_key = tostring(key) end",
    );
    assert_eq!(support::global_string(&mut original, "zero_key"), "-0");

    let mut loaded = support::reload(&original);
    support::run(
        &mut loaded,
        "for key in pairs(t) do zero_key_after_load = tostring(key) end; value_after_load = t[0.0]",
    );
    assert_eq!(
        support::global_string(&mut loaded, "zero_key_after_load"),
        "-0"
    );
    assert_eq!(support::global_number(&mut loaded, "value_after_load"), 2.0);
}

#[test]
fn large_table_pairs_order_round_trips() {
    let mut original = State::new();
    support::run(
        &mut original,
        r#"
        order = {}
        for i = 1, 30 do order["k" .. i] = i end
        keys = ""
        for k in pairs(order) do keys = keys .. k .. "," end
    "#,
    );
    let before = support::global_string(&mut original, "keys");

    let mut loaded = support::reload(&original);
    support::run(
        &mut loaded,
        "keys2 = \"\"\nfor k in pairs(order) do keys2 = keys2 .. k .. \",\" end",
    );
    assert_eq!(support::global_string(&mut loaded, "keys2"), before);
}

#[test]
fn corruption_sweep_never_panics() {
    let mut state = State::new();
    support::run(
        &mut state,
        r#"
        t = { a = 1, b = "two", c = { 3, 4, 5 } }
        t.self = t
        f = string.gsub
        counter = (function() local n = 1 return function() n = n + 1 return n end end)()
    "#,
    );
    let bytes = state.save_state().unwrap().bytes;

    // The decoder requires every field and exact input consumption. A proper
    // prefix must fail even though the format has no trailing integrity hash.
    fn ignore(_: Result<State, LoadError>) {}

    // Every proper prefix (each truncation length) must be rejected.
    for len in 0..bytes.len() {
        let result =
            State::load_state(&bytes[..len], Box::new(DefaultCallbacks), support::no_setup);
        assert!(result.is_err(), "truncated prefix of {len} bytes loaded");
    }
    // A single-byte flip at every offset loads or errors without a panic.
    for i in 0..bytes.len() {
        let mut corrupted = bytes.clone();
        corrupted[i] ^= 0xff;
        ignore(State::load_state(
            &corrupted,
            Box::new(DefaultCallbacks),
            support::no_setup,
        ));
    }
}

#[test]
fn same_bytecode_closures_and_binary_literals_survive_snapshot() {
    // Two closures of one factory Bytecode share a rebuilt runtime bundle on
    // load (internal sharing is pinned by a vm.rs unit test; this asserts the
    // observable half), and a non-UTF-8 string literal must round-trip
    // through the load-time interning path.
    let mut original = State::new();
    support::run(
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
    let mut loaded = support::reload(&original);
    support::run(
        &mut loaded,
        "r1 = add1(subject) r2 = add2(subject) r3 = add1(subject)",
    );
    assert_eq!(support::global_number(&mut loaded, "r1"), 41.0);
    assert_eq!(support::global_number(&mut loaded, "r2"), 42.0);
    assert_eq!(support::global_number(&mut loaded, "r3"), 41.0);
}

#[test]
fn table_library_extension_survives_snapshot() {
    let mut original = State::new();
    support::run(&mut original, "table.custom = function(t) return 17 end");
    let mut loaded = support::reload(&original);
    support::run(&mut loaded, "result = table.custom({})");
    assert_eq!(support::global_number(&mut loaded, "result"), 17.0);
}

#[test]
fn rebound_table_library_survives_snapshot() {
    let mut original = State::new();
    support::run(
        &mut original,
        "table = { custom = function(t) return 23 end }",
    );
    let mut loaded = support::reload(&original);
    support::run(&mut loaded, "result = table.custom({})");
    assert_eq!(support::global_number(&mut loaded, "result"), 23.0);
}

#[test]
fn table_library_metatable_survives_snapshot() {
    let mut original = State::new();
    support::run(
        &mut original,
        r#"
            setmetatable(table, { __index = function(_, key)
                if key == "custom" then return function() return 29 end end
            end })
        "#,
    );
    let mut loaded = support::reload(&original);
    support::run(&mut loaded, "result = table.custom()");
    assert_eq!(support::global_number(&mut loaded, "result"), 29.0);
}

#[test]
fn ordered_table_library_replay_restores_membership() {
    // Seven live keys with different membership plus a forced reorder, so the
    // load path takes the ordered environment replay.
    let mut original = State::new();
    support::run(
        &mut original,
        r#"
            table.insert = nil
            table.custom = function() return 31 end
            local remove = table.remove
            table.remove = nil
            table.remove = remove
        "#,
    );
    let mut loaded = support::reload(&original);
    support::run(
        &mut loaded,
        "result = table.custom() \
         insert_gone = (table.insert == nil) and 1 or 0 \
         plain_miss = (({}).custom == nil) and 1 or 0",
    );
    assert_eq!(support::global_number(&mut loaded, "result"), 31.0);
    assert_eq!(support::global_number(&mut loaded, "insert_gone"), 1.0);
    assert_eq!(support::global_number(&mut loaded, "plain_miss"), 1.0);
}

/// Array-part holes decide where later integer writes land and so the
/// `pairs()` order and `#`. Continuing after a save/load must match
/// continuing without one, for user tables and environment tables alike.
#[test]
fn array_part_shape_survives_round_trip() {
    let setup = r#"
        t = {}
        for i = 1, 6 do t[i] = i end
        t.name = "x"
        t[2] = nil t[3] = nil t[6] = nil
        for i = 1, 6 do math[i] = i end
        math[2] = nil math[3] = nil math[6] = nil
    "#;
    let after = r#"
        t[3] = 3 t[6] = 6 math[3] = 3 math[6] = 6
        local function order(tbl)
            local out = {}
            for k in pairs(tbl) do
                if type(k) == "number" or k == "name" then out[#out + 1] = tostring(k) end
            end
            return table.concat(out, ",")
        end
        result = order(t) .. "|" .. #t .. "|" .. order(math) .. "|" .. #math
    "#;
    let mut original = State::new();
    support::run(&mut original, setup);
    let mut loaded = support::reload(&original);
    support::run(&mut original, after);
    support::run(&mut loaded, after);
    let expected = support::global_string(&mut original, "result");
    assert_eq!(expected, "1,3,4,5,6,name|6|1,3,4,5,6|6");
    assert_eq!(support::global_string(&mut loaded, "result"), expected);
}

#[test]
fn environment_order_replay_preserves_setup_array_holes() {
    let mut original = State::new();
    support::run(&mut original, "math.floor = nil; math.floor = 7");
    let save = original.save_state().unwrap();
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        support::run(
            state,
            "math[1] = 1; math[2] = 2; math[3] = 3; math[2] = nil",
        );
        Ok::<(), std::convert::Infallible>(())
    })
    .expect("order replay preserves setup slots");
    support::run(
        &mut loaded,
        "math[2] = 2; order = ''; for k in pairs(math) do if type(k) == 'number' then order = order .. k end end",
    );
    assert_eq!(support::global_string(&mut loaded, "order"), "123");
}

#[test]
fn environment_order_replay_with_hash_only_numeric_keys() {
    // An environment table with no array part but numeric hash keys 3 and 2,
    // a forced reorder, and an entry only the load-time setup adds. The
    // replay rebuilds it with an empty array, so key 1 written afterwards
    // must still start the array exactly as it does without a save.
    let setup = "math[3] = 3; math[2] = 2; math.floor = nil; math.floor = 7";
    let after = "math[1] = 1; order = ''; for k in pairs(math) do \
                 if type(k) == 'number' then order = order .. k end end; \
                 length = #math";
    let mut original = State::new();
    support::run(&mut original, setup);
    let save = original.save_state().unwrap();
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        support::run(state, "math.extra = 5");
        Ok::<(), std::convert::Infallible>(())
    })
    .expect("order replay accepts hash-only numeric keys");
    support::run(&mut original, after);
    support::run(&mut loaded, after);
    assert_eq!(
        support::global_string(&mut loaded, "order"),
        support::global_string(&mut original, "order")
    );
    assert_eq!(
        support::global_number(&mut loaded, "length"),
        support::global_number(&mut original, "length")
    );
    support::run(&mut loaded, "extra = math.extra; floor = math.floor");
    assert_eq!(support::global_number(&mut loaded, "extra"), 5.0);
    assert_eq!(support::global_number(&mut loaded, "floor"), 7.0);
}

#[test]
fn environment_array_replay_preserves_setup_append_keys() {
    let mut original = State::new();
    support::run(&mut original, "math[1] = 1; math[2] = 2; math[2] = nil");
    let save = original.save_state().unwrap();
    let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |state| {
        support::run(state, "math[3] = 3");
        Ok::<(), std::convert::Infallible>(())
    })
    .expect("setup append key can extend the saved array");
    support::run(
        &mut loaded,
        "value = math[3]; length = #math; math[2] = 2; order = ''; for k in pairs(math) do if type(k) == 'number' then order = order .. k end end",
    );
    assert_eq!(support::global_number(&mut loaded, "value"), 3.0);
    assert_eq!(support::global_number(&mut loaded, "length"), 3.0);
    assert_eq!(support::global_string(&mut loaded, "order"), "123");
}

#[test]
fn load_discards_setup_stack_values() {
    let saved = State::new().save_state().unwrap();
    let loaded = State::load_state(&saved.bytes, Box::new(DefaultCallbacks), |state| {
        state.push_number(42.0)
    })
    .unwrap();
    assert_eq!(loaded.get_top(), 0);
}

#[test]
fn dead_environment_function_keys_have_reachability_paths() {
    fn host_fn(_: &mut State) -> dellingr::Result<u8> {
        Ok(0)
    }
    for shadow in [false, true] {
        let mut state = State::new();
        state.push_rust_fn(host_fn).unwrap();
        state.set_global("host_fn").unwrap();
        support::run(
            &mut state,
            "string[host_fn] = true; string[host_fn] = nil; host_fn = nil",
        );
        if shadow {
            support::run(&mut state, "string = nil");
        }
        match state.save_state().unwrap_err() {
            SaveError::UnregisteredFunction { reachable_from } => {
                if shadow {
                    assert_eq!(reachable_from, "environment string");
                } else {
                    assert!(reachable_from.starts_with("global string.key["));
                }
            }
            error => panic!("unexpected save error: {error}"),
        }
    }
}
