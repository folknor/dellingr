//! A field miss on a plain table is nil, as in reference Lua. The VM used to
//! fall through to the global `table` library on a miss, which made `t.k`
//! disagree with `t["k"]` and made every miss error once `table` was not a
//! table.

use dellingr::error::{ErrorKind, TypeError};
use dellingr::{ArgCount, LuaType, RetCount, State};

fn run_bool(state: &mut State, source: &str) -> bool {
    state.load_string(source).expect("test source compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .unwrap_or_else(|error| panic!("test source fails: {source}\n{error}"));
    let result = state.to_boolean(-1);
    state.pop(1).expect("result is present on the stack");
    result
}

#[test]
fn table_library_names_are_nil_on_plain_tables() {
    let mut state = State::new();
    assert!(run_bool(
        &mut state,
        r#"
            local t = {}
            return t.insert == nil and t.remove == nil and t.sort == nil
                and t.concat == nil and t.unpack == nil and t.pack == nil
                and t.move == nil and t.insert == t["insert"]
        "#,
    ));
}

#[test]
fn class_index_miss_does_not_reach_table_library() {
    let mut state = State::new();
    assert!(run_bool(
        &mut state,
        r#"
            local Class = {}
            Class.__index = Class
            local obj = setmetatable({}, Class)
            local fn_index = setmetatable({}, { __index = function() return nil end })
            return obj.remove == nil and fn_index.insert == nil
        "#,
    ));
}

#[test]
fn global_proxy_does_not_expose_table_library_names() {
    let mut state = State::new();
    assert!(run_bool(&mut state, "return _G.insert == nil"));
}

#[test]
fn misses_do_not_consult_a_customized_table_library() {
    let mut state = State::new();
    assert!(run_bool(
        &mut state,
        r#"
            table.custom = 7
            setmetatable(table, { __index = function()
                error("table library must not be consulted")
            end })
            local plain = {}
            local indexed = setmetatable({}, { __index = function() return nil end })
            return plain.custom == nil and plain.unknown == nil
                and indexed.custom == nil and indexed.unknown == nil
        "#,
    ));
}

#[test]
fn explicit_index_and_own_methods_still_work() {
    let mut state = State::new();
    assert!(run_bool(
        &mut state,
        r#"
            local own = { insert = function(_, value) return value end }
            local inherited = setmetatable({}, { __index = table })
            inherited:insert(9)
            return own:insert(7) == 7 and inherited[1] == 9
        "#,
    ));
}

#[test]
fn method_sugar_on_plain_table_is_an_error() {
    let mut state = State::new();
    state
        .load_string("local t = {} t:insert(1)")
        .expect("test source compiles");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("the missing insert field is not callable");
    assert!(matches!(
        error.kind(),
        ErrorKind::TypeError(TypeError::FunctionCall(LuaType::Nil))
    ));
}

#[test]
fn misses_survive_a_non_table_table_global() {
    for binding in ["nil", "5"] {
        let mut state = State::new();
        assert!(run_bool(
            &mut state,
            &format!(
                "table = {binding} local t = {{}} if t.x then return false end return t.y == nil"
            ),
        ));
    }
}

#[test]
fn misses_survive_a_restricted_environment_without_table() {
    let mut state = State::new();
    state
        .load_string("function probe() local t = {} return t.unknown == nil end")
        .expect("test source compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("setup runs");
    let restricted = state.with_restricted_env(&["probe"], |state| {
        state
            .get_global("probe")
            .expect("entry point is whitelisted");
        state.call(ArgCount::Fixed(0), RetCount::Fixed(1))?;
        let result = state.to_boolean(-1);
        state.pop(1).expect("result is present on the stack");
        dellingr::Result::Ok(result)
    });
    assert!(restricted.expect("plain-table miss must not error in a sandbox"));
}

#[test]
fn plain_miss_only_charges_for_table_creation() {
    let mut state = State::new();
    assert!(!run_bool(&mut state, "local t = {}; return t.unknown"));
    // NEW_TABLE costs one; GET_FIELD and RETURN are free.
    assert_eq!(state.cost_used(), 1);
}
