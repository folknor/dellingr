-- brokkr verdict workload derived from examples/calls/global.lua (see that
-- file for the probe rationale: a hot loop calling a user-defined global).
-- Same kernel, repeated K times per _bench call so one call is ~100ms and
-- the standalone run takes a few seconds in a release build.

function add_one(x)
    return x + 1
end

local function kernel()
    local sum = 0
    for i = 1, 1000 do
        sum = add_one(sum)
    end
    return sum
end

local K = 1750

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/global: true")
