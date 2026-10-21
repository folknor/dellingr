-- brokkr verdict workload derived from examples/calls/loop_beside_closure.lua
-- (see that file for the probe rationale). Same kernel, repeated K times per
-- _bench call so one call is ~100ms and the standalone run takes a few
-- seconds in a release build.

local function kernel()
    local scale = 3
    local scaled = function(v)
        return v * scale
    end
    local total = 0
    for i = 1, 2000 do
        local t = i * 2
        total = total + t
    end
    local j = 0
    while j < 1000 do
        local step = 1
        j = j + step
    end
    return scaled(total + j)
end

local K = 1200

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/loop_beside_closure: true")
