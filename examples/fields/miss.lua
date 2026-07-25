-- Hypothesis: a field read that MISSES on a plain table is far more expensive
-- than it looks. With no metatable and the key absent, the read still calls
-- push_table_library_field, which does a global lookup of `table` plus a full
-- lookup against the table library before finally returning nil. That fallback
-- exists only to support `t:insert(...)` sugar, so it can never resolve for
-- keys that are not table-library method names.
-- The absent keys below are deliberately NOT table-library names, which is the
-- overwhelmingly common case for the `if t.optional_field then` idiom.
-- Compare against fields/same_obj_read.lua, where every read hits.

local cfg = {
    width = 100,
    height = 50,
    depth = 25,
}

function _bench()
    local n = 0
    for i = 1, 1000 do
        if cfg.optional_scale then n = n + 1 end
        if cfg.optional_tint then n = n + 2 end
        if cfg.optional_label then n = n + 4 end
        n = n + cfg.width + cfg.height
    end
    return n
end

for i = 1, 700 do _bench() end
print("fields/miss: true")
