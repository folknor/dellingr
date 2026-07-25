-- brokkr verdict workload derived from examples/alloc/closure.lua (see
-- that file for the probe rationale). Same kernel, repeated K times per
-- _bench call so one call is ~100ms and the standalone run takes a few
-- seconds in a release build.

local function kernel()
    local total = 0
    for i = 1, 500 do
        local x = i
        local f = function(y)
            return x + y
        end
        total = total + f(1)
    end
    return total
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
print("bench/closure: true")
