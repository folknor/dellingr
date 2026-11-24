-- brokkr verdict workload derived from examples/calls/general.lua (see
-- that file for what it measures). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds in a
-- release build.

local function inc(x)
    return x + 1
end

local function mix(x)
    return inc(x) + inc(x + 1) + inc(x + 2)
end

local function kernel()
    local sum = 0
    for i = 1, 10000 do
        sum = sum + mix(i)
    end
    return sum
end

local K = 90

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/general: true")
