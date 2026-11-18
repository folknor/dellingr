-- A host callback decoding eight scalar fields of persistent entity records
-- each tick, in the same proportions in both workload files. Under
-- dellingr-bench `_host_decode` is a Rust function; elsewhere the Lua
-- fallback computes the same sum.
-- Harness host: records
local decode = _host_decode or function(e)
    local s = e.x + e.y + e.vx + e.vy + e.hp + e.player + #e.kind
    if e.alive then s = s + 1 end
    return s
end
local entities = {}
for i = 1, 64 do
    entities[i] = {x = i, y = i * 2, vx = 1, vy = -1, hp = 100,
                   player = i % 4, kind = "unit", alive = i % 2 == 0}
end
local function kernel()
    local sum = 0
    for _ = 1, 20 do
        for i = 1, #entities do sum = sum + decode(entities[i]) end
    end
    return sum
end
function _bench()
    local sum = 0
    for i = 1, 700 do sum = sum + kernel() end
    return sum
end
for i = 1, 30 do _bench() end
print("bench/host_records: true")
