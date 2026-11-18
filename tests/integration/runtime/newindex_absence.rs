//! A metatable that once answered "no `__newindex`" makes later stores into
//! its tables raw, with no presence lookup. Every route that can give the
//! metatable a live `__newindex` must end that, so the next absent-key store
//! reaches the handler.

use dellingr::State;

use crate::support;

/// Warm the absence through every store route, install a handler with
/// `install`, then check that every route reaches it.
fn check_install(install: &str) {
    let code = format!(
        r#"
        hits = 0
        function handler(t, k, v) hits = hits + 1 end
        mt = {{ __index = {{}} }}
        -- A metatable whose own metatable has no __newindex, used as a
        -- terminal __newindex table behind `chain`.
        store = setmetatable({{}}, mt)
        chain = setmetatable({{}}, {{ __newindex = store }})
        local function stores(n)
            local o = setmetatable({{}}, mt)
            o.field = n
            o[n] = n
            local key = "dyn" .. n
            o[key] = n
            chain["via" .. n] = n
            return o
        end
        for i = 1, 3 do
            local o = stores(i)
            if rawget(o, "field") ~= i or rawget(o, i) ~= i or rawget(o, "dyn" .. i) ~= i then
                error("warm stores were not raw")
            end
            if rawget(store, "via" .. i) ~= i then error("chain store missed the terminal table") end
        end
        if hits ~= 0 then error("no handler yet, but " .. hits .. " hits") end
        {install}
        local o = stores(10)
        if hits ~= 4 then error("expected 4 handler hits after install, got " .. hits) end
        if rawget(o, "field") ~= nil or rawget(o, 10) ~= nil or rawget(o, "dyn10") ~= nil then
            error("a store went raw past the handler")
        end
        if rawget(store, "via10") ~= nil then error("the chain went raw past the handler") end
        "#
    );
    let mut state = State::new();
    support::run(&mut state, &code);
}

#[test]
fn a_field_assignment_installs_the_handler() {
    check_install("mt.__newindex = handler");
}

#[test]
fn a_dynamic_key_installs_the_handler() {
    check_install(r#"local name = "__new" .. "index"; mt[name] = handler"#);
}

#[test]
fn rawset_installs_the_handler() {
    check_install(r#"rawset(mt, "__newindex", handler)"#);
}

#[test]
fn a_revived_handler_slot_installs_the_handler() {
    // The handler existed, was removed (a tombstone) and absence was learned
    // in between: the re-add revives or relocates the dead slot.
    for padding in ["", "for i = 1, 12 do mt['pad' .. i] = i end"] {
        check_install(&format!(
            "{padding}
             mt.__newindex = handler
             mt.__newindex = nil
             stores(20)
             if hits ~= 0 then error('the removed handler still ran') end
             mt.__newindex = handler"
        ));
    }
}

#[test]
fn a_global_table_metatable_revives_its_handler_in_place() {
    // `_G` revives a global in place, through the global write path rather
    // than a table insert.
    let mut state = State::new();
    support::run(
        &mut state,
        r#"
        hits = 0
        local function handler() hits = hits + 1 end
        local function set(v) __newindex = v end
        set(handler)
        set(nil)
        local function store(k)
            local o = setmetatable({}, _G)
            o[k] = 1
            o.field = 1
            return o
        end
        for i = 1, 3 do store("k" .. i) end
        if hits ~= 0 then error("no handler yet") end
        set(handler)
        local o = store("late")
        if hits ~= 2 or rawget(o, "late") ~= nil or rawget(o, "field") ~= nil then
            error("the revived global handler was skipped: " .. hits)
        end
        "#,
    );
}

#[test]
fn a_host_store_sees_a_handler_installed_after_absence_was_learned() {
    let mut state = State::new();
    support::run(
        &mut state,
        r#"
        hits = 0
        mt = {}
        obj = setmetatable({}, mt)
        obj.warm = 1
        mt.__newindex = function() hits = hits + 1 end
        "#,
    );
    state.get_global("obj").unwrap();
    state.push_string("late").unwrap();
    state.push_number(1.0).unwrap();
    state.set_table(1).unwrap();
    state.set_top(0).unwrap();
    support::run(
        &mut state,
        r#"if hits ~= 1 or rawget(obj, "late") ~= nil then error("host store went raw") end"#,
    );
}
