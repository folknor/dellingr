//! Field and method inline caches revalidate after the table, its metatable
//! or its `__index` table changes, and never cache a metamethod's result.

use dellingr::State;

use crate::support;

#[test]
fn cached_fields_do_not_resurrect_after_deletion() {
    // Each field callsite gets its own cache slot, so the access has to happen
    // inside a function called both before and after the deletion. Two separate
    // `t.x` expressions would never share a warm cache and would pass even if
    // the tombstone were resurrected.
    // Encodes both reads into one number so the warming read is checked too:
    // 1 before the deletion, 9 (via __index) after.
    assert_eq!(
        support::eval_number(
            r#"
            local t = setmetatable({ x = 1 }, { __index = function() return 9 end })
            local function read() return t.x end
            local warm = read()
            t.x = nil
            return warm * 100 + read()
        "#
        ),
        109.0
    );
    // Same for the write cache: the warming assignment must be non-nil and go
    // through the same callsite, so the IC records (key -> index) against a live
    // slot. Removal does not bump the table version, so a `set_at_index` that
    // failed to refuse dead slots would write straight through the tombstone and
    // never reach __newindex.
    // The warming write must land on the live field directly (leaving seen at 0
    // and t.x at 5); only the post-deletion write should reach __newindex. The
    // encoding keeps the intermediate state observable: 0 * 1000 + 5 * 100 + 7.
    assert_eq!(
        support::eval_number(
            r#"
            local seen = 0
            local t = setmetatable({ x = 1 }, { __newindex = function(_, _, v) seen = v end })
            local function write(v) t.x = v end
            write(5)
            local warmed_seen = seen
            local warmed_field = t.x
            t.x = nil
            write(7)
            return warmed_seen * 1000 + warmed_field * 100 + seen
        "#
        ),
        507.0
    );
}

#[test]
fn cached_fields_follow_readded_keys_and_compaction() {
    expect_ok(
        r#"
        local t = { a = 1, b = 2, c = 3, d = 4, e = 5, f = 6 }
        local function read() return t.b end
        local function write(v) t.b = v end
        write(20)
        if read() ~= 20 then error('warming access failed') end

        t.b = nil
        t.b = 21
        if read() ~= 21 then error('read used vacated slot') end
        write(22)
        if read() ~= 22 then error('write used vacated slot') end
        if t.c ~= 3 then error('write overwrote adjacent key') end

        -- Leave b live while deleting enough keys to compact on the next add.
        t.a = nil
        t.c = nil
        t.d = nil
        t.e = nil
        t.g = 7
        if read() ~= 22 then error('read used pre-compaction index') end
        write(23)
        if read() ~= 23 then error('write used pre-compaction index') end
        if t.f ~= 6 or t.g ~= 7 then error('write overwrote compacted key') end
        local order = ''
        for key in pairs(t) do order = order .. key end
        if order ~= 'fbg' then error('wrong order after compaction') end
    "#,
    );
}

#[test]
fn field_cache_revalidates_after_table_mutation() {
    let val = support::eval_number(
        r#"
        local t = { a = 1, b = 2, c = 3 }
        local function read_b()
            return t.b
        end

        local first = read_b()
        t.a = nil
        local second = read_b()
        t.b = 7
        local third = read_b()
        return first * 100 + second * 10 + third
    "#,
    );
    assert_eq!(val, 227.0);
}

#[test]
fn field_cache_does_not_cache_index_metamethod_result() {
    let val = support::eval_number(
        r#"
        local calls = 0
        local t = setmetatable({}, {
            __index = function(self, key)
                calls = calls + 1
                return calls
            end
        })
        return t.x + t.x
    "#,
    );
    assert_eq!(val, 3.0);
}

#[test]
fn repeated_field_updates_store_the_final_value() {
    let val = support::eval_number(
        r#"
        local t = { count = 0 }
        for i = 1, 100 do
            t.count = t.count + 1
        end
        return t.count
    "#,
    );
    assert_eq!(val, 100.0);
}

#[test]
fn set_field_cache_revalidates_after_remove() {
    let val = support::eval_number(
        r#"
        local t = { a = 1, b = 2, c = 3 }
        local function set_b(v)
            t.b = v
        end

        set_b(20)
        local first = t.b
        t.a = nil
        set_b(200)
        local second = t.b
        return first * 1000 + second
    "#,
    );
    assert_eq!(val, 20200.0);
}

#[test]
fn set_field_cache_does_not_skip_newindex_for_new_key() {
    let val = support::eval_number(
        r#"
        local backing = {}
        local hits = 0
        local t = setmetatable({}, {
            __newindex = function(_, k, v)
                hits = hits + 1
                rawset(backing, k, v)
            end
        })
        for i = 1, 5 do
            t.x = i
        end
        return hits * 100 + backing.x
    "#,
    );
    assert_eq!(val, 505.0);
}

#[test]
fn set_field_cache_handles_assigning_nil_as_delete() {
    let val = support::eval_number(
        r#"
        local t = { x = 1, y = 2, z = 3 }
        t.y = nil
        local count = 0
        for _ in pairs(t) do
            count = count + 1
        end
        return count
    "#,
    );
    assert_eq!(val, 2.0);
}

#[test]
fn method_cache_revalidates_index_table_method_update() {
    let val = support::eval_number(
        r#"
        local methods = {}
        function methods:f()
            return 1
        end

        local t = setmetatable({}, { __index = methods })
        local first = t:f()

        function methods:f()
            return 2
        end

        local second = t:f()
        return first * 10 + second
    "#,
    );
    assert_eq!(val, 12.0);
}

#[test]
fn method_cache_revalidates_index_table_reassignment() {
    let val = support::eval_number(
        r#"
        local methods_a = {}
        local methods_b = {}
        function methods_a:f()
            return 1
        end
        function methods_b:f()
            return 2
        end

        local mt = { __index = methods_a }
        local t = setmetatable({}, mt)
        local first = t:f()
        mt.__index = methods_b
        local second = t:f()
        return first * 10 + second
    "#,
    );
    assert_eq!(val, 12.0);
}

#[test]
fn method_cache_does_not_cache_index_function_result() {
    let val = support::eval_number(
        r#"
        local calls = 0
        local t = setmetatable({}, {
            __index = function(self, key)
                calls = calls + 1
                return function()
                    return calls
                end
            end
        })
        return t:f() + t:f()
    "#,
    );
    assert_eq!(val, 3.0);
}

fn expect_ok(code: &str) {
    let mut state = State::new();
    support::run(&mut state, code);
}
