//! `_G` is the global table itself: the raw primitives see the real globals,
//! keys are byte-exact, and its metatable is protected.

use dellingr::State;

use crate::support;

/// Run `source` on a fresh standard State.
fn run(source: &str) -> dellingr::Result<()> {
    support::try_run(&mut State::new(), source)
}

#[test]
fn distinct_non_utf8_keys_do_not_alias() {
    run(r#"
        _G["\xff"] = 1
        if _G["\xfe"] ~= nil then error("\\xfe aliased \\xff") end
        if _G["\xef\xbf\xbd"] ~= nil then error("U+FFFD aliased \\xff") end
        if _G["\xff"] ~= 1 then error("\\xff lost") end
        _G["\xef\xbf\xbd"] = 2
        if _G["\xff"] ~= 1 then error("U+FFFD write clobbered \\xff") end
        _G["\xff"] = nil
        if _G["\xff"] ~= nil then error("\\xff not cleared") end
        if _G["\xef\xbf\xbd"] ~= 2 then error("U+FFFD lost") end
    "#)
    .expect("non-UTF-8 _G keys stay distinct");
}

#[test]
fn utf8_keys_still_reach_globals() {
    run(r#"
        _G["caf\xc3\xa9"] = 3
        _G.plain = 4
        if plain ~= 4 then error("_G.plain not a global") end
        if _G["caf\xc3\xa9"] ~= 3 then error("UTF-8 key lost") end
        x = 5
        if _G.x ~= 5 then error("global not visible through _G") end
    "#)
    .expect("UTF-8 _G keys name globals");
}

#[test]
fn global_env_metatable_is_protected() {
    run(r#"
        if getmetatable(_G) ~= false then error("getmetatable(_G) is not false") end
        x = 1
        if _G.x ~= 1 then error("_G broken") end
    "#)
    .expect("getmetatable(_G) is falsy");
    let err = run("setmetatable(_G, nil)").expect_err("setmetatable(_G) must fail");
    assert!(
        err.to_string().contains("protected metatable"),
        "unexpected error: {err}"
    );
}

#[test]
fn nil_assignment_to_unknown_global_reads_nil() {
    run(r#"
        never_set = nil
        if never_set ~= nil then error("never_set not nil") end
        if _G.never_set ~= nil then error("_G.never_set not nil") end
        g = 1
        g = nil
        if g ~= nil then error("g not cleared") end
        g = 2
        if g ~= 2 then error("g not re-set") end
    "#)
    .expect("nil globals read as nil");
}

#[test]
fn g_updates_keep_builtin_and_cached_global_reads_coherent() {
    run(r#"
        local original_type = type
        counter = 1
        local function read_counter() return counter end
        if read_counter() ~= 1 then error("initial global lost") end
        _G.counter = 2
        if read_counter() ~= 2 then error("cached global stale") end
        _G.type = function() return "replacement" end
        if type(1) ~= "replacement" then error("builtin read stale") end
        _G.type = original_type
        if type(1) ~= "number" then error("builtin restore failed") end
        _G.counter = nil
        if read_counter() ~= nil then error("global deletion stale") end
    "#)
    .expect("_G writes update builtin and cached bindings");
}

#[test]
fn g_is_the_global_table_for_the_raw_primitives() {
    run(r##"
        if rawget(_G, "print") ~= print then error("rawget missed print") end
        if rawget(_G, "_G") ~= _G or _G._G ~= _G then error("_G is not its own entry") end

        rawset(_G, "x", 1)
        if x ~= 1 or _G.x ~= 1 then error("rawset did not define x") end
        x = 2
        if _G.x ~= 2 or rawget(_G, "x") ~= 2 then error("global write split from _G") end
        _G.x = 3
        if x ~= 3 then error("_G write split from the global") end
        rawset(_G, "x", nil)
        if x ~= nil then error("rawset nil did not delete x") end

        local found = {}
        for k, v in pairs(_G) do found[k] = v end
        if found.print ~= print or found.string ~= string or found._G ~= _G then
            error("pairs(_G) misses globals")
        end
        local k = next(_G)
        if k == nil then error("next(_G) found nothing") end
        if #_G ~= 0 then error("#_G counts string keys") end
        table.insert(_G, "first")
        if rawget(_G, 1) ~= "first" or #_G ~= 1 then error("table.insert on _G") end
    "##)
    .expect("the raw primitives see the real globals");
}

#[test]
fn pairs_g_keeps_a_cleared_and_redefined_global_in_place() {
    run(r#"
        first = 1; second = 2; third = 3
        second = nil
        second = 22
        local order = {}
        for k in pairs(_G) do
            if k == "first" or k == "second" or k == "third" then
                order[#order + 1] = k
            end
        end
        if table.concat(order, ",") ~= "first,second,third" then
            error("order: " .. table.concat(order, ","))
        end
    "#)
    .expect("a redefined global keeps its place");
}

#[test]
fn restricted_environment_contents_are_the_g_table_while_they_run() {
    let mut state = State::new();
    support::try_run(&mut state, "outer = 1; saved_g = _G").unwrap();
    state
        .with_restricted_env(&["saved_g", "rawset", "rawget", "error"], |state| {
            // The reference taken outside sees the restricted environment,
            // and raw keys written there stay there.
            support::try_run(
                state,
                r#"
                if rawget(saved_g, "outer") ~= nil then error("outer leaked in") end
                rawset(saved_g, "inner", 1)
                rawset(saved_g, "\xff", 2)
                if inner ~= 1 then error("raw write is not a global") end
                "#,
            )
            .unwrap();
        })
        .unwrap();
    support::try_run(
        &mut state,
        r#"
        if inner ~= nil or _G["\xff"] ~= nil then error("restricted keys leaked out") end
        if outer ~= 1 or saved_g ~= _G then error("outer environment changed") end
        "#,
    )
    .unwrap();
}

#[test]
fn a_raw_state_has_a_global_table_without_g() {
    let mut state = State::raw();
    state.get_global("_G").unwrap();
    assert_eq!(state.typ(-1), dellingr::LuaType::Nil);
    state.pop(1).unwrap();
    state.push_number(7.0).unwrap();
    state.set_global("seven").unwrap();
    state.get_global("seven").unwrap();
    assert_eq!(state.to_number(-1).unwrap(), 7.0);
}
