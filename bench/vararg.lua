-- brokkr verdict workload derived from examples/calls/vararg.lua (see
-- that file for what it measures). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds in a
-- release build.

local function sum_varargs(...)
    local total = 0
    for _, v in ipairs({...}) do
        total = total + v
    end
    return total
end

local function kernel()
    local total = 0
    for i = 1, 500 do
        total = total + sum_varargs(1, 2, 3, 4)
    end
    return total
end

local K = 800

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/vararg: true")
