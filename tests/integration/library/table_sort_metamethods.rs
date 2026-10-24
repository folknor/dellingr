//! `table.sort` follows Lua 5.4's metamethod rules (`__len`, `__index`,
//! `__newindex`) for proxy tables and keeps the raw path for plain ones.

use dellingr::{ArgCount, RetCount, State};

use crate::support;

#[test]
fn proxy_sorts_backing_store_through_metamethods() {
    let code = r#"
        local store = {5, 2, 9, 1, 7, 3}
        local reads, writes = 0, 0
        local proxy = setmetatable({}, {
            __len = function() return #store end,
            __index = function(_, k) reads = reads + 1; return store[k] end,
            __newindex = function(_, k, v) writes = writes + 1; store[k] = v end,
        })
        table.sort(proxy)
        for i = 1, 5 do
            if store[i] > store[i + 1] then error("not sorted at " .. i) end
        end
        if reads == 0 or writes ~= 6 then error("metamethods not used") end
        if next(proxy) ~= nil then error("proxy gained raw elements") end
        return store[1] * 1000 + store[6]
    "#;
    assert_eq!(support::eval_number(code), 1009.0);
}

#[test]
fn proxy_sort_with_comparator_descending() {
    let code = r#"
        local store = {"b", "d", "a", "c"}
        local proxy = setmetatable({}, {
            __len = function() return #store end,
            __index = store,
            __newindex = store,
        })
        table.sort(proxy, function(a, b) return a > b end)
        if table.concat(store) ~= "dcba" then error(table.concat(store)) end
        return 1
    "#;
    assert_eq!(support::eval_number(code), 1.0);
}

#[test]
fn proxy_sort_is_deterministic_and_matches_raw_order() {
    let code = r#"
        local function build()
            local t = {}
            for i = 1, 40 do t[i] = {k = (i * 7) % 5, id = i} end
            return t
        end
        local cmp = function(a, b) return a.k < b.k end
        local raw = build()
        table.sort(raw, cmp)
        local store = build()
        local proxy = setmetatable({}, {
            __len = function() return #store end,
            __index = store,
            __newindex = store,
        })
        table.sort(proxy, cmp)
        for i = 1, 40 do
            if raw[i].id ~= store[i].id then error("order differs at " .. i) end
        end
        return 1
    "#;
    assert_eq!(support::eval_number(code), 1.0);
}

#[test]
fn proxy_sort_roots_scratch_values_across_handler_and_comparator_gc() {
    let mut state = State::new();
    state
        .push_rust_fn(|state| {
            state.gc_collect();
            Ok(0)
        })
        .unwrap();
    state.set_global("force_gc").unwrap();
    support::run(
        &mut state,
        r#"
            local store = {}
            local proxy = setmetatable({}, {
                __len = function() force_gc(); return 12 end,
                __index = function(_, k)
                    force_gc()
                    return {key = 13 - k, label = "value " .. (13 - k)}
                end,
                __newindex = function(_, k, v)
                    force_gc()
                    if v.label ~= "value " .. v.key then error("write value lost") end
                    store[k] = v
                end,
            })
            table.sort(proxy, function(a, b)
                force_gc()
                if a.label ~= "value " .. a.key then error("left value lost") end
                if b.label ~= "value " .. b.key then error("right value lost") end
                return a.key < b.key
            end)
            for i = 1, 12 do
                if store[i].key ~= i then error("scratch value lost or unsorted") end
            end
            "#,
    );
}

#[test]
fn proxy_sort_comparison_error_for_mixed_types() {
    let mut state = State::new();
    support::load(
        &mut state,
        r#"
            local store = {1, "x", 3}
            local proxy = setmetatable({}, {
                __len = function() return #store end,
                __index = store,
                __newindex = store,
            })
            table.sort(proxy)
            "#,
    );
    let err = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("mixed types do not compare");
    assert!(err.to_string().contains("attempt to compare"), "{err}");
}

#[test]
fn proxy_sort_charges_like_raw_sort() {
    let plain = support::cost_of("local t = {5, 2, 9, 1, 7, 3, 8}; table.sort(t)");
    let proxied = support::cost_of(
        "local t = {5, 2, 9, 1, 7, 3, 8}; \
         setmetatable(t, {__newindex = {}}); \
         table.sort(t)",
    );
    // setmetatable is a separate call, so allow exactly its flat cost.
    let baseline =
        support::cost_of("local t = {5, 2, 9, 1, 7, 3, 8}; setmetatable(t, {__newindex = {}})");
    let base_plain = support::cost_of("local t = {5, 2, 9, 1, 7, 3, 8}");
    assert_eq!(proxied - baseline, plain - base_plain);
}

#[test]
fn table_with_unrelated_metatable_keeps_raw_sort() {
    let code = r#"
        local t = setmetatable({3, 1, 2}, {__tostring = function() return "x" end})
        table.sort(t)
        return t[1] * 100 + t[2] * 10 + t[3]
    "#;
    assert_eq!(support::eval_number(code), 123.0);
}
