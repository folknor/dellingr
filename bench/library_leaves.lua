-- brokkr verdict workload derived from examples/calls/library_leaves.lua (see
-- that file for the probe rationale: Lua calls of standard-library leaf
-- functions). Same kernel, repeated K times per _bench call so one call is
-- ~100ms and the standalone run takes a few seconds in a release build.

local floor = math.floor
local abs = math.abs

local function kernel()
    local t = { 1, 2, 3 }
    local sum = 0
    for i = 1, 400 do
        local x = i * 0.37
        sum = sum + floor(x) + math.floor(x + 0.5) + abs(-x) + math.sqrt(i)
        sum = sum + math.max(x, 7) + math.random(1)
        if type(x) == "number" and rawequal(t, t) then
            sum = sum + rawget(t, 2) + tonumber(i)
        end
    end
    return floor(sum)
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
print("bench/library_leaves: true")
