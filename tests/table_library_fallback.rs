//! Regression coverage for the plain-table table-library fallback gate.

use dellingr::{ArgCount, RetCount, State};

fn run_bool(state: &mut State, source: &str) -> bool {
    state
        .load_string(source)
        .expect("fallback test source compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .unwrap_or_else(|error| panic!("fallback test source fails: {source}\n{error}"));
    let result = state.to_boolean(-1);
    state.pop(1).expect("result is present on the stack");
    result
}

#[test]
fn plain_tables_resolve_every_table_library_member() {
    let mut state = State::new();
    assert!(run_bool(
        &mut state,
        r#"
            local t = {}
            local insert = t.insert
            t:insert(3)
            local remove = t.remove
            t:remove()
            local sort = t.sort
            t:sort()
            local unpack = t.unpack
            local unused = t:unpack()
            local pack = t.pack
            local packed = t:pack(1, 2)
            local concat = t.concat
            local joined = t:concat()
            local move = t.move
            local moved = t:move(1, 0, 1)
            return type(insert) == "function" and type(remove) == "function"
                and type(sort) == "function" and type(unpack) == "function"
                and type(pack) == "function" and type(concat) == "function"
                and type(move) == "function" and type(packed) == "table"
                and moved == t
        "#,
    ));
}

#[test]
fn extracted_table_library_member_keeps_working() {
    let mut state = State::new();
    assert!(run_bool(
        &mut state,
        "local t = {}; local f = t.insert; f(t, 7); return t[1] == 7",
    ));
}

#[test]
fn replaced_and_extended_table_library_members_are_observed() {
    let mut state = State::new();
    assert!(run_bool(
        &mut state,
        r#"
            table.insert = function(t, value) t.replaced = value end
            table.custom = function(t, value) t.custom_value = value end
            local t = {}
            t:insert(3)
            t:custom(4)
            return t.replaced == 3 and t.custom_value == 4 and type(t.custom) == "function"
        "#,
    ));
}

#[test]
fn rebound_table_and_non_table_bindings_keep_existing_behavior() {
    let mut state = State::new();
    assert!(run_bool(
        &mut state,
        "table = { custom = function(t) return 9 end }; return ({}):custom() == 9",
    ));

    let mut state = State::new();
    state
        .load_string("table = 1; local t = {}; return t.unknown")
        .expect("non-table fallback source compiles");
    assert!(state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).is_err());
}

#[test]
fn canonical_table_metatable_handles_unknown_fallback_keys() {
    let mut state = State::new();
    assert!(run_bool(
        &mut state,
        r#"
            setmetatable(table, { __index = function(_, key)
                if key == "unknown" then return function() return 12 end end
            end })
            return ({}):unknown() == 12
        "#,
    ));
}

#[test]
fn restricted_environment_rejects_fallback_then_restores_it() {
    let mut state = State::new();
    state
        .load_string("function fallback_miss() local t = {}; return t.unknown end")
        .expect("restricted-environment source compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("restricted-environment setup runs");

    let restricted = state.with_restricted_env(&["fallback_miss"], |state| {
        state
            .get_global("fallback_miss")
            .expect("entry point is whitelisted");
        state.call(ArgCount::Fixed(0), RetCount::Fixed(1))
    });
    assert!(restricted.is_err());

    state
        .get_global("fallback_miss")
        .expect("entry point is restored");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("fallback works after environment restoration");
    assert!(!state.to_boolean(-1));
}

#[test]
fn rebind_then_gc_never_uses_the_stale_cache_object() {
    let mut state = State::new();
    state
        .load_string(
            "function rebind() table = { custom = function() return 5 end } end \
             function probe() return ({}):custom() end",
        )
        .expect("rebind source compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("rebind setup runs");
    state.get_global("rebind").expect("rebind function exists");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("table rebind succeeds");
    state.gc_collect();
    state.get_global("probe").expect("probe function exists");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect("fallback resolves through rebound table");
    assert_eq!(state.to_number(-1).expect("probe result is numeric"), 5.0);
}

#[test]
fn pristine_unknown_miss_keeps_its_exact_cost() {
    let mut state = State::new();
    assert!(!run_bool(&mut state, "local t = {}; return t.unknown"));
    assert_eq!(state.cost_used(), 1);
}
