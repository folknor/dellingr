//! A field miss on a plain table is nil, as in reference Lua. The VM used to
//! fall through to the global `table` library on a miss, which made `t.k`
//! disagree with `t["k"]` and made every miss error once `table` was not a
//! table.

use dellingr::error::{ErrorKind, TypeError};
use dellingr::{ArgCount, LuaType, RetCount, State};

use crate::support;

/// The truthiness of what `source` returns on `state`, popped again.
fn run_bool(state: &mut State, source: &str) -> bool {
    support::eval(state, source, 1);
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
            for _, name in ipairs({"insert", "remove", "sort", "concat", "unpack", "pack", "move"}) do
                if t[name] ~= nil then error("t." .. name .. " is not nil on a plain table") end
            end
            if t.insert ~= t["insert"] then error("t.insert disagrees with t['insert']") end
            return true
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
            if obj.remove ~= nil then error("a class miss reached table.remove") end
            if fn_index.insert ~= nil then error("a function __index miss reached table.insert") end
            return true
        "#,
    ));
}

#[test]
fn global_table_does_not_expose_table_library_names() {
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
            if plain.custom ~= nil then error("plain.custom consulted the customized table library") end
            if plain.unknown ~= nil then error("plain.unknown is not nil") end
            if indexed.custom ~= nil then error("indexed.custom consulted the customized table library") end
            if indexed.unknown ~= nil then error("indexed.unknown is not nil") end
            return true
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
            if own:insert(7) ~= 7 then error("an own insert method did not run") end
            if inherited[1] ~= 9 then error("the inherited table.insert did not append") end
            return true
        "#,
    ));
}

#[test]
fn method_sugar_on_plain_table_is_an_error() {
    let mut state = State::new();
    support::load(&mut state, "local t = {} t:insert(1)");
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
                "table = {binding} local t = {{}} if t.x then error('t.x is not nil') end if t.y ~= nil then error('t.y is not nil') end return true"
            ),
        ));
    }
}

#[test]
fn misses_survive_a_restricted_environment_without_table() {
    let mut state = State::new();
    support::run(
        &mut state,
        "function probe() local t = {} if t.unknown ~= nil then error('t.unknown is not nil') end return true end",
    );
    let restricted = state
        .with_restricted_env(&["probe"], |state| {
            state
                .get_global("probe")
                .expect("entry point is whitelisted");
            state.call(ArgCount::Fixed(0), RetCount::Fixed(1))?;
            let result = state.to_boolean(-1);
            state.pop(1).expect("result is present on the stack");
            dellingr::Result::Ok(result)
        })
        .and_then(std::convert::identity);
    assert!(restricted.expect("plain-table miss must not error in a sandbox"));
}

#[test]
fn plain_miss_only_charges_for_table_creation() {
    let mut state = State::new();
    assert!(!run_bool(&mut state, "local t = {}; return t.unknown"));
    // NEW_TABLE costs one; GET_FIELD and RETURN are free.
    assert_eq!(state.cost_used(), 1);
}

#[test]
fn repeated_field_misses_observe_own_writes_and_metatable_changes() {
    let mut state = State::new();
    assert!(run_bool(
        &mut state,
        r#"
            local mt = { __len = function() return 0 end }
            local t = setmetatable({}, mt)
            local function read() return t.optional end
            if read() ~= nil or read() ~= nil then error("the first misses are not nil") end
            t.optional = 11
            if read() ~= 11 then error("an own write was not observed") end
            t.optional = nil
            if read() ~= nil then error("removing the own value was not observed") end
            mt.__index = { optional = 12 }
            if read() ~= 12 then error("a table __index was not observed") end
            mt.__index = nil
            if read() ~= nil then error("clearing __index was not observed") end
            mt.__index = function() return 13 end
            if read() ~= 13 then error("a function __index was not observed") end
            mt.__index = nil
            if read() ~= nil then error("clearing a function __index was not observed") end
            setmetatable(t, { __index = { optional = 14 } })
            if read() ~= 14 then error("a replaced metatable was not observed") end
            setmetatable(t, nil)
            if read() ~= nil then error("removing the metatable was not observed") end
            t.optional = 15
            if read() ~= 15 then error("the final own write was not observed") end
            return true
        "#,
    ));
}

#[test]
fn inherited_field_misses_observe_method_writes_and_handler_replacement() {
    let mut state = State::new();
    assert!(run_bool(
        &mut state,
        r#"
            local methods = {}
            local mt = { __index = methods }
            local t = setmetatable({}, mt)
            local function read() return t.optional end
            if read() ~= nil or read() ~= nil then error("the first misses are not nil") end
            methods.optional = 21
            if read() ~= 21 then error("an inherited method write was not observed") end
            methods.optional = nil
            if read() ~= nil then error("removing the inherited value was not observed") end
            mt.__index = { optional = 22 }
            if read() ~= 22 then error("a replaced handler was not observed") end
            t.optional = 23
            if read() ~= 23 then error("an own write was not observed") end
            return true
        "#,
    ));
}

#[cfg(feature = "snapshot")]
#[test]
fn warmed_missing_field_observes_writes_after_snapshot_load() {
    let mut state = State::new();
    assert!(run_bool(
        &mut state,
        r#"
            receiver = setmetatable({}, { __len = function() return 0 end })
            function read_optional() return receiver.optional end
            if read_optional() ~= nil or read_optional() ~= nil then error("the warm-up misses are not nil") end
            return true
        "#,
    ));
    let save = state.save_state().expect("state saves");
    let mut loaded = State::load_state(
        &save.bytes,
        Box::new(dellingr::DefaultCallbacks),
        support::no_setup,
    )
    .expect("save loads");
    assert!(run_bool(
        &mut loaded,
        r#"
            if read_optional() ~= nil then error("the loaded miss is not nil") end
            getmetatable(receiver).__index = { optional = 41 }
            if read_optional() ~= 41 then error("a handler installed after load was not observed") end
            receiver.optional = 42
            if read_optional() ~= 42 then error("an own write after load was not observed") end
            return true
        "#,
    ));
}
