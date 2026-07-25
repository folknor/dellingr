-- brokkr verdict workload derived from examples/numerics/arithmetic.lua
-- (see that file for the probe rationale). Same kernel, repeated K times
-- per _bench call so one call is ~100ms and the standalone run takes a
-- few seconds in a release build.

local function fib(n)
    if n < 2 then return n end
    return fib(n - 1) + fib(n - 2)
end

local function kernel()
    local sum = 0.0
    for i = 1, 50000 do
        sum = sum + i * 2.5 - i / 3.0
    end
    return sum + fib(20)
end

local K = 26

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/arithmetic: true")
