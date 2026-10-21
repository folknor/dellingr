//! The shared string metatable.
//!
//! Strings index through one metatable whose `__index` is the string library
//! table `open_libs` built, as in reference Lua: `getmetatable("")` returns
//! it, the global `string` binding plays no part in `s:method()`, and a script
//! assigning its `__index` redirects string indexing. These tests also pin
//! that the string-method inline cache notices such an assignment.

use dellingr::{ArgCount, RetCount, State};

fn run(state: &mut State, source: &str) {
    state.load_string(source).unwrap();
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .unwrap_or_else(|e| panic!("error running:\n{source}\n{e}"));
}

fn run_fresh(source: &str) {
    run(&mut State::new(), source);
}

fn run_err(source: &str) -> String {
    let mut state = State::new();
    state.load_string(source).unwrap();
    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("script must fail");
    err.to_string()
}

#[test]
fn getmetatable_of_a_string_is_shared_and_indexes_the_library() {
    run_fresh(
        r#"
        local mt = getmetatable("")
        if type(mt) ~= "table" then error("no string metatable") end
        if mt.__index ~= string then error("__index is not the string library") end
        if getmetatable("abc") ~= mt then error("metatable is not shared") end
        if getmetatable("").__metatable ~= nil then error("unexpected __metatable") end
        "#,
    );
}

#[test]
fn setmetatable_on_a_string_still_fails() {
    let err = run_err(r#"setmetatable("x", {})"#);
    assert!(err.contains("table expected"), "{err}");
}

#[test]
fn methods_survive_rebinding_or_clearing_the_global_string() {
    run_fresh(
        r#"
        local function f() return ("a"):upper() end
        if f() ~= "A" then error("warm") end
        local saved = string
        string = { upper = function() return "REBOUND" end }
        if f() ~= "A" then error("cached site followed the rebound global") end
        if ("b"):upper() ~= "B" then error("cold site followed the rebound global") end
        string = nil
        if f() ~= "A" then error("string = nil broke method syntax") end
        string = saved
        "#,
    );
}

#[test]
fn assigning_index_redirects_warm_and_cold_sites() {
    run_fresh(
        r#"
        local mt = getmetatable("")
        local lib = mt.__index
        local function f(s) return s:upper() end
        for _ = 1, 3 do if f("x") ~= "X" then error("warm") end end
        mt.__index = { upper = function(s) return "custom " .. s end }
        if f("x") ~= "custom x" then error("warm cache missed the __index write") end
        if ("y"):upper() ~= "custom y" then error("cold site") end
        if ("y").len ~= nil then error("old library still visible") end
        mt.__index = function(s, k) return s .. ":" .. k end
        if ("a").b ~= "a:b" then error("function __index") end
        if ("q")["upper"] ~= "q:upper" then error("function __index via GET_TABLE") end
        mt.__index = lib
        if f("z") ~= "Z" then error("restoring the library") end
        "#,
    );
}

#[test]
fn function_index_redirects_a_warm_method_site_and_receives_chained_strings() {
    run_fresh(
        r#"
        local mt = getmetatable("")
        local function upper(s) return s:upper() end
        if upper("a") ~= "A" then error("warm") end
        mt.__index = function(s, key)
            if key == "upper" then
                if s ~= "b" then error("wrong method receiver") end
                return function(receiver) return receiver .. "!" end
            end
            return s .. ":" .. key
        end
        if upper("b") ~= "b!" then error("warm method kept old handler") end
        local proxy = setmetatable({}, {__index = "handler"})
        if proxy.field ~= "handler:field" then error("wrong chained receiver") end
        if proxy["key"] ~= "handler:key" then error("wrong dynamic chained receiver") end
        "#,
    );
}

#[test]
fn cached_handler_is_not_reused_after_it_is_collected() {
    let mut state = State::new();
    run(
        &mut state,
        r#"
        local mt = getmetatable("")
        mt.__index = { upper = function() return "old" end }
        function cached() return ("x"):upper() end
        if cached() ~= "old" then error("warm") end
        mt.__index = {}
        "#,
    );
    // The old handler and closure have no Lua or canonical environment roots.
    state.gc_collect();
    run(
        &mut state,
        r#"
        for i = 1, 40 do local unused = {upper = i} end
        getmetatable("").__index = {upper = function() return "new" end}
        if cached() ~= "new" then error("cache used a collected handler") end
        "#,
    );
}

#[test]
fn library_mutation_is_seen_by_warm_sites() {
    run_fresh(
        r#"
        local function f(s) return s:upper() end
        if f("a") ~= "A" then error("warm") end
        string.upper = function() return "patched" end
        if f("a") ~= "patched" then error("library value write not seen") end
        string.shout = function(s) return s .. "!" end
        if ("hi"):shout() ~= "hi!" then error("new library key not seen") end
        "#,
    );
}

#[test]
fn nil_index_makes_string_indexing_an_error() {
    let err = run_err(
        r#"
        getmetatable("").__index = nil
        local _ = ("a").upper
        "#,
    );
    assert!(err.contains("attempt to index a string value"), "{err}");
}

#[test]
fn nil_index_invalidates_a_warm_site() {
    let mut state = State::new();
    run(
        &mut state,
        r#"
        function cached() return ("a").upper end
        if cached() ~= string.upper then error("warm") end
        getmetatable("").__index = nil
        "#,
    );
    state.get_global("cached").unwrap();
    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(1))
        .expect_err("warm cache must respect the missing handler");
    assert!(
        err.to_string().contains("attempt to index a string value"),
        "{err}"
    );
}

#[test]
fn removing_and_readding_index_after_a_warm_hit() {
    // A shape change on the metatable (key removed, other keys added) must
    // never let the cache read a slot that now holds something else.
    run_fresh(
        r#"
        local mt = getmetatable("")
        local lib = mt.__index
        local function f(s) return s:upper() end
        if f("a") ~= "A" then error("warm") end
        mt.__index = nil
        for i = 1, 40 do mt["k" .. i] = lib end
        mt.__index = { upper = function() return "new" end }
        if f("a") ~= "new" then error("stale slot after reshape") end
        "#,
    );
}

#[test]
fn restricted_env_without_string_keeps_method_syntax() {
    let mut state = State::new();
    run(
        &mut state,
        r#"function call_upper() return ("hi"):upper() end"#,
    );
    // Exercise a site warmed outside the restricted environment as well as
    // repeated lookups inside it.
    state.get_global("call_upper").unwrap();
    state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
    assert_eq!(state.to_string(-1).unwrap(), "HI");
    state.pop(1).unwrap();
    for _ in 0..2 {
        let result = state
            .with_restricted_env(&["call_upper"], |state| {
                state.get_global("call_upper").unwrap();
                state.call(ArgCount::Fixed(0), RetCount::Fixed(1)).unwrap();
                let s = state.to_string(-1).unwrap();
                state.pop(1).unwrap();
                s
            })
            .expect("state is not poisoned");
        assert_eq!(result, "HI");
    }
    // The global `string` is still hidden inside the sandbox.
    let hidden = state
        .with_restricted_env(&[], |state| {
            state.get_global("string").unwrap();
            let nil = state.typ(-1) == dellingr::LuaType::Nil;
            state.pop(1).unwrap();
            nil
        })
        .expect("state is not poisoned");
    assert!(hidden);
}

#[test]
fn string_metatable_survives_gc_when_unreferenced() {
    let mut state = State::new();
    run(&mut state, "string = nil");
    state.gc_collect();
    run(
        &mut state,
        r#"if ("ok"):upper() ~= "OK" then error("library collected") end"#,
    );
}

#[test]
fn empty_state_has_no_string_metatable() {
    let mut state = State::empty();
    state.push_string("x").unwrap();
    state.get_metatable_of(-1).unwrap();
    assert_eq!(state.typ(-1), dellingr::LuaType::Nil);
}

#[cfg(feature = "snapshot")]
mod snapshot {
    use super::run;
    use dellingr::{DefaultCallbacks, State};

    fn reload(state: &mut State) -> State {
        let save = state.save_state().unwrap();
        State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap()
    }

    #[test]
    fn held_metatable_reloads_as_the_live_object() {
        let mut state = State::new();
        run(&mut state, r#"held = getmetatable("")"#);
        let mut loaded = reload(&mut state);
        run(
            &mut loaded,
            r#"
            if held ~= getmetatable("") then error("identity lost") end
            if held.__index ~= string then error("__index lost") end
            "#,
        );
    }

    #[test]
    fn index_change_round_trips() {
        let mut state = State::new();
        run(
            &mut state,
            r#"
            custom = { upper = function(s) return "custom " .. s end }
            getmetatable("").__index = custom
            "#,
        );
        let mut loaded = reload(&mut state);
        run(
            &mut loaded,
            r#"
            if getmetatable("").__index ~= custom then error("__index change lost") end
            if ("x"):upper() ~= "custom x" then error("method did not follow") end
            "#,
        );
    }

    #[test]
    fn index_delta_round_trips_without_a_global_reference() {
        let mut state = State::new();
        run(
            &mut state,
            r#"
            getmetatable("").__index = {upper = function(s) return s .. "!" end}
            string = nil
            "#,
        );
        state.gc_collect();
        let mut loaded = reload(&mut state);
        loaded.gc_collect();
        run(
            &mut loaded,
            r#"
            if string ~= nil then error("shadow lost") end
            if ("x"):upper() ~= "x!" then error("metatable delta lost") end
            "#,
        );
    }
}
