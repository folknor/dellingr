-- brokkr verdict workload derived from examples/tables/column_key.lua (see
-- that file for what it measures). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds in a
-- release build.

local units = {}
for i = 1, 64 do
    units[i] = {
        hp = 100 - i,
        armor = i % 9,
        speed = i % 7 + 1,
        range = i % 4 * 10,
        level = i % 12,
        team = i % 2,
    }
end
local columns = { "hp", "armor", "speed", "range", "level", "team" }

local function kernel()
    local us, cs = units, columns
    local total = 0
    for round = 1, 10 do
        for c = 1, 6 do
            local key = cs[c]
            local sum = 0
            for i = 1, 64 do
                sum = sum + us[i][key]
            end
            total = total + sum * c
        end
    end
    return total
end

local K = 1000

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/column_key: true")
