use dellingr::{ArgCount, RetCount, State};

use crate::support;

fn call_writer(state: &mut State, name: &str, value: f64) {
    state.get_global(name).expect("writer exists");
    state.push_number(value).expect("argument fits");
    state
        .call(ArgCount::Fixed(1), RetCount::Fixed(0))
        .expect("writer runs");
}

#[test]
fn writer_closures_update_the_same_global() {
    let mut state = State::new();
    support::run(
        &mut state,
        "function make() return function(v) cache_target = v end end a = make(); b = make()",
    );
    call_writer(&mut state, "a", 1.0);
    call_writer(&mut state, "b", 2.0);
    call_writer(&mut state, "a", 3.0);
    assert_eq!(support::global_number(&mut state, "cache_target"), 3.0);
}

#[test]
fn writer_updates_global_across_restricted_environment_swaps() {
    let mut state = State::new();
    support::run(&mut state, "x = 0; function writer(v) x = v end");
    call_writer(&mut state, "writer", 1.0);
    state
        .with_restricted_env(&["writer"], |restricted| {
            call_writer(restricted, "writer", 2.0);
            assert_eq!(support::global_number(restricted, "x"), 2.0);
        })
        .expect("state is not poisoned");
    assert_eq!(support::global_number(&mut state, "x"), 1.0);
    call_writer(&mut state, "writer", 3.0);
    assert_eq!(support::global_number(&mut state, "x"), 3.0);
}

#[test]
fn unrelated_and_g_table_writes_preserve_writer_behavior() {
    let mut state = State::new();
    support::run(&mut state, "x = 0; function writer(v) x = v end");
    call_writer(&mut state, "writer", 1.0);
    support::run(&mut state, "y = 2; _G.y = 3");
    call_writer(&mut state, "writer", 4.0);
    assert_eq!(support::global_number(&mut state, "x"), 4.0);
    assert_eq!(support::global_number(&mut state, "y"), 3.0);
}

#[test]
fn builtin_rebinding_preserves_writer_behavior() {
    let mut state = State::new();
    support::run(&mut state, "x = 0; function writer(v) x = v end");
    call_writer(&mut state, "writer", 1.0);
    support::run(
        &mut state,
        "table = { marker = 7 }; builtin_marker = table.marker; g_table_marker = _G.table.marker",
    );
    call_writer(&mut state, "writer", 2.0);
    assert_eq!(support::global_number(&mut state, "builtin_marker"), 7.0);
    assert_eq!(support::global_number(&mut state, "g_table_marker"), 7.0);
    assert_eq!(support::global_number(&mut state, "x"), 2.0);
}

#[test]
fn global_writes_remain_cost_free() {
    let mut state = State::new();
    // The adds must involve a local so constant folding leaves all three as
    // runtime (costed) operations; the global write itself must charge 0.
    support::run(
        &mut state,
        "function writer(v) x = v + 1 + 1 + 1 end; writer(1)",
    );
    assert_eq!(state.cost_used(), 3);
}

#[cfg(feature = "snapshot")]
#[test]
fn new_save_with_cached_set_global_round_trips() {
    let mut state = State::new();
    support::run(&mut state, "function writer(v) saved_x = v end; writer(1)");
    let save = state.save_state().expect("state saves");
    let mut loaded = State::load_state(
        &save.bytes,
        Box::new(dellingr::DefaultCallbacks),
        support::no_setup,
    )
    .expect("new save loads");
    call_writer(&mut loaded, "writer", 2.0);
    assert_eq!(support::global_number(&mut loaded, "saved_x"), 2.0);
}

#[test]
fn warmed_method_route_keeps_table_reference_across_global_swaps() {
    let mut state = State::new();
    support::run(
        &mut state,
        "local methods = { value = 7 }; string = methods; \
         local receiver = setmetatable({}, { __index = string }); \
         function read_method() method_value = receiver.value end; read_method(); \
         string = nil; read_method()",
    );
    assert_eq!(support::global_number(&mut state, "method_value"), 7.0);
    state
        .with_restricted_env(&["read_method"], |state| {
            support::run(state, "read_method()");
            assert_eq!(support::global_number(state, "method_value"), 7.0);
        })
        .unwrap();
    support::run(&mut state, "read_method()");
    assert_eq!(support::global_number(&mut state, "method_value"), 7.0);
}

#[test]
fn repeated_absent_global_reads_observe_later_definitions() {
    let mut state = State::new();
    support::run(
        &mut state,
        "function read_optional() observed = optional_value end; \
         read_optional(); read_optional(); optional_value = 31; read_optional()",
    );
    assert_eq!(support::global_number(&mut state, "observed"), 31.0);
    support::run(
        &mut state,
        "optional_value = nil; read_optional(); read_optional(); \
         _G.optional_value = 32; read_optional()",
    );
    assert_eq!(support::global_number(&mut state, "observed"), 32.0);
}

#[test]
fn restricted_global_miss_does_not_hide_restored_binding() {
    let mut state = State::new();
    support::run(
        &mut state,
        "optional_value = 33; \
         function read_optional() observed = optional_value end; read_optional()",
    );
    state
        .with_restricted_env(&["read_optional"], |restricted| {
            support::run(restricted, "read_optional(); read_optional()");
            restricted.get_global("observed").expect("lookup fits");
            assert_eq!(restricted.typ(-1), dellingr::LuaType::Nil);
            restricted.pop(1).expect("lookup result pops");
        })
        .expect("state is not poisoned");
    support::run(&mut state, "read_optional()");
    assert_eq!(support::global_number(&mut state, "observed"), 33.0);
}

#[cfg(feature = "snapshot")]
#[test]
fn absent_global_reader_observes_definition_after_snapshot_load() {
    let mut state = State::new();
    support::run(
        &mut state,
        "function read_optional() observed = optional_value end; \
         read_optional(); read_optional()",
    );
    let save = state.save_state().expect("state saves");
    let mut loaded = State::load_state(
        &save.bytes,
        Box::new(dellingr::DefaultCallbacks),
        support::no_setup,
    )
    .expect("save loads");
    support::run(
        &mut loaded,
        "read_optional(); optional_value = 34; read_optional()",
    );
    assert_eq!(support::global_number(&mut loaded, "observed"), 34.0);
}
