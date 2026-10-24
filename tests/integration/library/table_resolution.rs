use dellingr::{ArgCount, RetCount, State};

use crate::support;

/// A budget that runs out inside a raw `table.move` copies exactly the
/// elements it paid for, one unit each, before failing - the outcome the
/// per-element charge produces - rather than failing before any copy.
#[test]
fn budget_exhaustion_mid_move_keeps_the_paid_prefix() {
    let mut state = State::new();
    support::run(
        &mut state,
        "src = {} for i = 1, 100 do src[i] = i end dst = {}",
    );

    support::load(&mut state, "table.move(src, 1, 100, 1, dst)");
    state.set_cost_budget(50);
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("a 100-element move cannot fit a 50-unit budget");
    assert!(
        error.to_string().contains("budget"),
        "expected a budget error, got: {error}"
    );
    let used = state.cost_used();

    state.set_cost_budget(i64::MAX);
    state.get_global("dst").expect("dst is a global");
    let copied = state.raw_len(-1).expect("dst is a table");
    assert!(
        copied > 0 && copied < 100,
        "expected a partial copy, got {copied} elements"
    );
    assert!(
        u64::try_from(copied).expect("small count") <= used,
        "each copied element was charged"
    );
}

#[test]
fn move_and_unpack_accept_the_same_exact_singleton_key() {
    assert!(support::eval_boolean(
        r#"
        local t = {}
        table.move(t, 2^54, 2^54, 1)
        return table.unpack(t, 2^54, 2^54) == nil
        "#
    ));
}

#[test]
fn raw_and_metamethod_sorts_share_order_and_comparator_calls() {
    assert!(support::eval_boolean(
        r#"
        local function values()
            return {
                { key = 2, id = "a" },
                { key = 1, id = "b" },
                { key = 1, id = "c" },
                { key = 3, id = "d" },
                { key = 2, id = "e" }
            }
        end
        local function comparator(log)
            return function(a, b)
                log[#log + 1] = a.id .. b.id
                return a.key < b.key
            end
        end

        local raw = values()
        local raw_log = {}
        table.sort(raw, comparator(raw_log))

        local backing = values()
        local proxy = setmetatable({}, {
            __len = function() return #backing end,
            __index = function(_, key) return backing[key] end,
            __newindex = function(_, key, value) backing[key] = value end
        })
        local meta_log = {}
        table.sort(proxy, comparator(meta_log))

        if #raw_log ~= #meta_log then error("sort comparator counts differ") end
        for i = 1, #raw_log do
            if raw_log[i] ~= meta_log[i] then error("sort comparator calls differ") end
        end
        for i = 1, #raw do
            if raw[i].id ~= backing[i].id then error("sort orders differ") end
        end
        return true
        "#
    ));
}

#[test]
fn nil_writes_to_existing_metatable_keys_keep_removal_order_and_dispatch() {
    assert!(support::eval_boolean(
        r#"
        local function exercise(count)
            local t = {10, 20}
            for i = 1, count do t["k" .. i] = i end
            local calls = 0
            setmetatable(t, {
                __newindex = function(self, key, value)
                    calls = calls + 1
                    rawset(self, key, value)
                end
            })
            t[1] = nil
            t.k1 = nil
            if calls ~= 0 or t[1] ~= nil or t.k1 ~= nil then return false end
            -- A tombstone remains a valid next control key.
            local successor = next(t, "k1")
            if count > 1 and successor ~= "k2" then return false end
            t.k1 = 99
            if calls ~= 1 then return false end
            local expected = 2
            for key, value in pairs(t) do
                if expected == 2 then
                    if key ~= 2 or value ~= 20 then return false end
                elseif expected <= count + 1 then
                    local i = expected - 1
                    if key ~= "k" .. i or value ~= i then return false end
                else
                    if key ~= "k1" or value ~= 99 then return false end
                end
                expected = expected + 1
            end
            return expected == count + 3
        end
        return exercise(1) and exercise(24)
        "#
    ));
}
