-- brokkr verdict workload derived from examples/alloc/static_records.lua
-- (see that file for the probe rationale). Same kernel, repeated K times
-- per _bench call so one call is ~100ms and the standalone run takes a
-- few seconds in a release build.

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

local function kernel()
    local sum = 0
    for i = 1, 500 do
        local u = new_unit(i, i, i)
        sum = sum + u.id + u.hp
    end
    return sum
end

local K = 320

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/static_records: true")
