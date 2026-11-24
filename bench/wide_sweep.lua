-- brokkr verdict workload derived from examples/fields/wide_sweep.lua (see
-- that file for what it measures). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds in a
-- release build.

local records = {}
for i = 1, 4096 do
    records[i] = {
        x = i,
        y = i * 2,
        vx = i % 5,
        vy = i % 3,
        hp = 100 - i % 50,
        armor = i % 9,
        team = i % 2,
        level = i % 12,
    }
end
local count = #records

local function kernel()
    local rs, n = records, count
    local sum = 0
    local i = 1
    while i <= n do
        local r = rs[i]
        sum = sum + r.x + r.vy + r.hp + r.level
        i = i + 1
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
print("bench/wide_sweep: true")
