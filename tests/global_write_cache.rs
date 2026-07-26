use dellingr::{ArgCount, RetCount, State};

fn run(state: &mut State, source: &str) {
    state.load_string(source).expect("source compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("source runs");
}

fn call_writer(state: &mut State, name: &str, value: f64) {
    state.get_global(name).expect("writer exists");
    state.push_number(value).expect("argument fits");
    state
        .call(ArgCount::Fixed(1), RetCount::Fixed(0))
        .expect("writer runs");
}

fn global_number(state: &mut State, name: &str) -> f64 {
    state.get_global(name).expect("global exists");
    let value = state.to_number(-1).expect("global is a number");
    state.pop(1).expect("result pops");
    value
}

#[test]
fn warm_writer_inserts_populates_then_hits_and_closures_share_the_site() {
    let mut state = State::new();
    run(
        &mut state,
        "function make() return function(v) cache_target = v end end a = make(); b = make()",
    );
    call_writer(&mut state, "a", 1.0);
    call_writer(&mut state, "b", 2.0);
    call_writer(&mut state, "a", 3.0);
    assert_eq!(global_number(&mut state, "cache_target"), 3.0);
}

#[test]
fn writer_repopulates_across_restricted_environment_swaps() {
    let mut state = State::new();
    run(&mut state, "x = 0; function writer(v) x = v end");
    call_writer(&mut state, "writer", 1.0);
    state.with_restricted_env(&["writer"], |restricted| {
        call_writer(restricted, "writer", 2.0);
        assert_eq!(global_number(restricted, "x"), 2.0);
    });
    assert_eq!(global_number(&mut state, "x"), 1.0);
    call_writer(&mut state, "writer", 3.0);
    assert_eq!(global_number(&mut state, "x"), 3.0);
}

#[test]
fn append_and_g_proxy_writes_do_not_displace_a_warmed_global() {
    let mut state = State::new();
    run(&mut state, "x = 0; function writer(v) x = v end");
    call_writer(&mut state, "writer", 1.0);
    run(&mut state, "y = 2; _G.y = 3");
    call_writer(&mut state, "writer", 4.0);
    assert_eq!(global_number(&mut state, "x"), 4.0);
    assert_eq!(global_number(&mut state, "y"), 3.0);
}

#[test]
fn builtin_rebind_invalidates_its_fallback_without_disturbing_a_writer() {
    let mut state = State::new();
    run(&mut state, "x = 0; function writer(v) x = v end");
    call_writer(&mut state, "writer", 1.0);
    run(
        &mut state,
        "table = { marker = 7 }; fallback_marker = ({}).marker",
    );
    call_writer(&mut state, "writer", 2.0);
    assert_eq!(global_number(&mut state, "fallback_marker"), 7.0);
    assert_eq!(global_number(&mut state, "x"), 2.0);
}

#[test]
fn global_writes_remain_cost_free() {
    let mut state = State::new();
    run(
        &mut state,
        "function writer() x = 1 + 1 + 1 + 1 end; writer()",
    );
    assert_eq!(state.cost_used(), 3);
}

#[cfg(feature = "snapshot")]
#[test]
fn new_save_with_cached_set_global_round_trips() {
    let mut state = State::new();
    run(&mut state, "function writer(v) saved_x = v end; writer(1)");
    let save = state.save_state().expect("state saves");
    let mut loaded = State::load_state(&save.bytes, Box::new(dellingr::DefaultCallbacks), |_| {})
        .expect("new save loads");
    call_writer(&mut loaded, "writer", 2.0);
    assert_eq!(global_number(&mut loaded, "saved_x"), 2.0);
}
