//! Caller slot positions survive hostile stack manipulation and nested calls.

use dellingr::{ArgCount, RetCount, State};

use crate::support;
use crate::support::PushNewRustFn;

fn churn(state: &mut State) -> dellingr::Result<u8> {
    let below = -(state.get_top() as isize) - 1;
    assert!(state.remove(below).is_err());
    assert!(state.replace(below).is_err());
    assert!(state.insert(below).is_err());
    assert!(state.copy_val(1, below).is_err());
    assert!(state.pop(state.get_top() as isize + 1).is_err());
    assert!(state.set_top(below - 1).is_err());
    // Force buffer growth, then remove every visible argument. Caller slots
    // must remain present even though their backing allocation may move.
    state.set_top(2048)?;
    state.set_top(0)?;
    dellingr::Program::compile(
        "local function many() return 7, 8 end; return {many()}, many()",
        None,
    )
    .and_then(|program| state.load(&program))?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))?;
    state.push_number(41.0)?;
    state.push_number(42.0)?;
    Ok(2)
}

/// Run `source`, which must leave the host stack empty.
fn run(state: &mut State, source: &str) {
    support::run(state, source);
    assert_eq!(state.get_top(), 0);
}

#[test]
fn caller_operands_survive_rust_metamethods_and_iterators() {
    let mut state = State::new();
    state.push_new_rust_fn(churn).unwrap();
    state.set_global("churn").unwrap();
    run(
        &mut state,
        r#"
        local sentinel = 99
        local t = setmetatable({}, {__index = churn, __newindex = churn})
        t.field = 5
        t[12] = 6
        if t.field ~= 41 or t[12] ~= 41 then error("receiver slot moved") end
        for a, b in churn, nil, nil do
            if a ~= 41 or b ~= 42 then error("iterator slots moved") end
            break
        end
        local lua_t = setmetatable({}, {
            __index = function(t, k) return churn(t, k) end,
            __newindex = function(t, k, v) churn(t, k, v) end
        })
        lua_t.field = 5
        lua_t[12] = 6
        if lua_t.field ~= 41 or lua_t[12] ~= 41 then
            error("Lua metamethod receiver slot moved")
        end
        local function iterator(s, c) return churn(s, c) end
        for a, b in iterator, nil, nil do
            if a ~= 41 or b ~= 42 then error("Lua iterator slots moved") end
            break
        end
        local function collect(...) return {...} end
        local values = collect(1, churn())
        if values[1] ~= 1 or values[2] ~= 41 or values[3] ~= 42 then
            error("outer call base lost")
        end
        local values = {1, churn()}
        if values[1] ~= 1 or values[2] ~= 41 or values[3] ~= 42 then
            error("outer constructor base lost")
        end
        if sentinel ~= 99 then error("caller local changed") end
    "#,
    );
}

#[test]
fn failed_calls_and_constructors_leave_no_stale_window() {
    let mut state = State::new();
    for source in [
        "local function f(...) end; f(1, error('stop'))",
        "local t = {1, error('stop')}",
        "local function f() local t = {error('stop')} end; local t = {f()}",
    ] {
        support::load(&mut state, source);
        assert!(state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).is_err());
        state.set_top(0).unwrap();
        run(
            &mut state,
            r#"
            local function many() return 2, 3 end
            local function collect(...) return {...} end
            local t = {1, many()}
            local u = collect(1, many())
            if t[3] ~= 3 or u[3] ~= 3 then error("stale call window") end
        "#,
        );
    }
}
