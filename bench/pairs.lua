-- brokkr verdict workload derived from examples/iter/pairs.lua (see that
-- file for the probe rationale). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds
-- in a release build.

local hash = {}
for i = 1, 2500 do
    hash["k_" .. i] = i
end

local function kernel()
    local sum = 0
    for _, v in pairs(hash) do
        sum = sum + v
    end
    return sum
end

local K = 700

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/pairs: true")
