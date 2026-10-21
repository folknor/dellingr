-- brokkr verdict workload derived from examples/alloc/list_literals.lua
-- (see that file for the probe rationale). Same kernel, repeated K times
-- per _bench call so one call is ~100ms and the standalone run takes a
-- few seconds in a release build.

local function kernel()
    local sum = 0
    for i = 1, 500 do
        local list = { i, i + 1, i + 2, i + 3, i + 4, i + 5, i + 6, i + 7, i + 8, i + 9 }
        local mixed = { i, i + 1, i + 2, i + 3, i + 4, i + 5, a = i, b = i, c = i, d = i, e = i }
        sum = sum + list[1] + list[10] + #list + mixed[6] + mixed.e + #mixed
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
print("bench/list_literals: true")
