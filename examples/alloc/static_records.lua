-- Workload: record literals whose field values are all static sources:
-- parameters, captured locals and constants, the usual shape of a game
-- object constructor. Every value is a register read, an upvalue read or a
-- literal load, so the whole constructor after its allocation is pinned
-- writes and free loads. Compare alloc/record_tables.lua, whose values are
-- computed between the writes.

local default_team = "blue"

local function new_unit(id, x, y)
    return {
        id = id,
        x = x,
        y = y,
        vx = 0,
        vy = 0,
        hp = 100,
        alive = true,
        team = default_team,
    }
end

function _bench()
    local sum = 0
    for i = 1, 500 do
        local u = new_unit(i, i, i)
        sum = sum + u.id + u.hp
    end
    return sum
end

for i = 1, 900 do _bench() end
print("alloc/static_records: " .. tostring(_bench()) .. ": true")
