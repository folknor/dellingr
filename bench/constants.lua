-- brokkr verdict workload derived from examples/numerics/constants.lua
-- (see that file for the probe rationale: no constant folding today, so
-- literal arithmetic re-executes per hit). Same kernel, repeated K times
-- per _bench call so one call is ~100ms and the standalone run takes a
-- few seconds in a release build.

local function kernel()
    local sum = 0
    for i = 1, 1000 do
        sum = sum + 60 * 1000
        sum = sum - 24 * 60
        sum = sum + -5
        sum = sum + 2 * 3 * 7
        sum = sum + 1024 / 4
    end
    return sum
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
print("bench/constants: true")
