-- Probe: a host callback decoding eight scalar fields of persistent entity
-- records (map-stored: more fields than the inline capacity) each tick.
-- Under dellingr-bench `_host_decode` is a Rust function reading the fields
-- with `State::raw_field`; elsewhere (the corpus, reference Lua) the Lua
-- fallback below computes the same sum, so the result is the same.
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
function _bench()
    local sum = 0
    for _ = 1, 20 do
        for i = 1, #entities do sum = sum + decode(entities[i]) end
    end
    return sum
end
for i = 1, 30 do _bench() end
print("calls/host_records: " .. tostring(_bench()) .. ": true")
