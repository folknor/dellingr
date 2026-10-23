//! `_G` proxy behaviour: byte-exact string keys and a protected metatable.

use dellingr::{ArgCount, RetCount, State};

fn run(source: &str) -> dellingr::Result<()> {
    let mut state = State::new();
    dellingr::Program::compile(source, None).and_then(|program| state.load(&program))?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
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
fn proxy_updates_keep_builtin_and_cached_global_reads_coherent() {
    run(r#"
        local original_type = type
        counter = 1
        local function read_counter() return counter end
        if read_counter() ~= 1 then error("initial global lost") end
        _G.counter = 2
        if read_counter() ~= 2 then error("cached global stale") end
        _G.type = function() return "replacement" end
        if type(1) ~= "replacement" then error("builtin mirror stale") end
        _G.type = original_type
        if type(1) ~= "number" then error("builtin restore failed") end
        _G.counter = nil
        if read_counter() ~= nil then error("global deletion stale") end
    "#)
    .expect("proxy writes update builtin mirrors and cached bindings");
}
