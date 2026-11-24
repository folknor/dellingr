-- brokkr verdict workload derived from examples/tables/key_sweep.lua (see
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
    local round = 1
    repeat
        local c = 1
        repeat
            local key = cs[c]
            local sum = 0
            local i = 1
            repeat
                sum = sum + us[i][key]
                i = i + 1
            until i > 64
            total = total + sum * c
            c = c + 1
        until c > 6
        round = round + 1
    until round > 10
    return total
end

local K = 1100

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/key_sweep: true")
