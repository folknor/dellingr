//! A field read through more than one `__index` table is cached as a route
//! that is not re-read on a hit: a heap-wide route epoch, moved by any write
//! to a table the route went through, stands in for it. Each test warms one
//! site through a two-level chain, changes one table on the route by one
//! kind of write, and checks the site answers what the heap now says.

use dellingr::State;

use crate::support;

/// Run `body` after the shared fixture: `read()` is one `obj.m` site whose
/// answer comes from `Base` through `Sub` and `SubMeta`, already warmed.
fn after_warm_route(body: &str) {
    let source = format!(
        r#"
            Base = {{ m = "base" }}
            Base.__index = Base
            SubMeta = {{ __index = Base }}
            Sub = setmetatable({{}}, SubMeta)
            Sub.__index = Sub
            obj = setmetatable({{}}, Sub)
            local function read() return obj.m end
            for _ = 1, 3 do
                if read() ~= "base" then error("the warm-up read is wrong") end
            end
            {body}
            return true
        "#
    );
    let mut state = State::new();
    support::eval(&mut state, &source, 1);
    assert!(state.to_boolean(-1));
}

#[test]
fn a_route_observes_a_value_write_in_its_terminal_table() {
    after_warm_route(
        r#"
            Base.m = "changed"
            if read() ~= "changed" then error("terminal write missed") end
            Base.m = nil
            if read() ~= nil then error("terminal removal missed") end
        "#,
    );
}

#[test]
fn a_route_observes_a_key_shadowing_it_in_an_intermediate_table() {
    after_warm_route(
        r#"
            Sub.m = "sub"
            if read() ~= "sub" then error("shadowing write missed") end
            Sub.m = nil
            if read() ~= "base" then error("unshadowing missed") end
            rawset(Sub, "m", "raw")
            if read() ~= "raw" then error("raw shadowing missed") end
        "#,
    );
}

#[test]
fn a_route_observes_its_links_changing() {
    after_warm_route(
        r#"
            SubMeta.__index = { m = "other" }
            if read() ~= "other" then error("relinked handler missed") end
            setmetatable(Sub, nil)
            if read() ~= nil then error("removed metatable missed") end
            setmetatable(Sub, { __index = function() return "fn" end })
            if read() ~= "fn" then error("function handler missed") end
        "#,
    );
}

#[test]
fn a_route_observes_its_receiver() {
    after_warm_route(
        r#"
            obj.m = "own"
            if read() ~= "own" then error("own write missed") end
            obj.m = nil
            setmetatable(obj, Base)
            if read() ~= "base" then error("one-level receiver missed") end
            setmetatable(obj, nil)
            if read() ~= nil then error("plain receiver missed") end
        "#,
    );
}

#[test]
fn a_route_through_the_global_table_observes_global_stores() {
    after_warm_route(
        r#"
            local env_obj = setmetatable({}, { __index = setmetatable({}, { __index = _G }) })
            local function global_read() return env_obj.route_probe end
            route_probe = 1
            for _ = 1, 3 do
                if global_read() ~= 1 then error("warm-up through _G is wrong") end
            end
            route_probe = 2
            if global_read() ~= 2 then error("cached global store missed") end
            route_probe = nil
            if global_read() ~= nil then error("global removal missed") end
        "#,
    );
}

#[test]
fn a_route_does_not_answer_after_its_epoch_is_moved_by_an_unrelated_table() {
    after_warm_route(
        r#"
            local Other = setmetatable({}, { __index = { n = 1 } })
            Other.__index = Other
            local other = setmetatable({}, Other)
            local function other_read() return other.n end
            if other_read() ~= 1 or other_read() ~= 1 then error("second route is wrong") end
            Base.m = "after"
            if read() ~= "after" or other_read() ~= 1 then error("routes disagree after a write") end
        "#,
    );
}
