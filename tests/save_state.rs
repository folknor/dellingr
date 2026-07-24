#![cfg(feature = "snapshot")]

use std::sync::{Arc, Mutex};

use dellingr::{ArgCount, DefaultCallbacks, HostCallbacks, LoadError, RetCount, SaveError, State};

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
    loaded.get_global("result");
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
    loaded.get_global("a");
    assert_eq!(loaded.to_number(-1).unwrap(), 4.0);
    loaded.pop(1);
    loaded.get_global("b");
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
fn unregistered_reachable_rust_function_fails_save() {
    fn host_fn(_state: &mut State) -> dellingr::Result<u8> {
        Ok(0)
    }

    let mut state = State::new();
    state.push_rust_fn(host_fn);
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
    state.push_number(1.0);
    let anchor = state.anchor().unwrap();

    let save = state.save_state().unwrap();
    assert_eq!(save.diagnostics.anchor_count, 1);

    let loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {}).unwrap();
    assert_eq!(loaded.anchor_count(), 0);

    assert!(state.release_anchor(anchor));
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
        state.push_number(42.0);
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
    state.get_global(name);
    let n = state.to_number(-1).unwrap();
    state.pop(1);
    n
}

fn global_str(state: &mut State, name: &str) -> String {
    state.get_global(name);
    let s = state.to_string_with_meta(-1).unwrap();
    state.pop(1);
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
fn non_utf8_and_edge_values_round_trip() {
    let mut state = fresh();

    state.push_bytes([0u8, 159, 146, 150, 255]);
    state.set_global("binval");
    state.push_number(-0.0);
    state.set_global("negzero");
    state.push_number(f64::NAN);
    state.set_global("nan");

    // Table keyed by a non-UTF8 byte string.
    state.new_table();
    state.push_bytes([0xff, 0x00, 0xfe]);
    state.push_number(42.0);
    state.set_table_raw(-3).unwrap();
    state.set_global("bt");

    let mut loaded = reload(&state);

    loaded.get_global("binval");
    assert_eq!(loaded.to_bytes(-1).unwrap(), [0u8, 159, 146, 150, 255]);
    loaded.pop(1);

    loaded.get_global("negzero");
    assert_eq!(loaded.to_number(-1).unwrap().to_bits(), (-0.0f64).to_bits());
    loaded.pop(1);

    loaded.get_global("nan");
    assert!(loaded.to_number(-1).unwrap().is_nan());
    loaded.pop(1);

    loaded.get_global("bt");
    loaded.push_bytes([0xff, 0x00, 0xfe]);
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
