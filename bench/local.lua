-- brokkr verdict workload derived from examples/calls/local.lua (see
-- that file for what it measures). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds in a
-- release build.

local function add_one(x)
    return x + 1
end

local function kernel()
    local sum = 0
    local f = add_one
    for i = 1, 1000 do
        sum = f(sum)
    end
    return sum
end

local K = 5000

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/local: true")
