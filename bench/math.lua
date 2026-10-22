-- brokkr verdict workload derived from examples/numerics/math.lua (see that
-- file for what it tracks). Same kernel, repeated K times per _bench call so
-- one call is ~100ms and the standalone run takes a few seconds in a release
-- build.

local function kernel()
    local acc = 0
    for i = 1, 200 do
        local a = i * 0.0314159
        local s, c = math.sin(a), math.cos(a)
        acc = acc + s * c + math.tan(a * 0.25)
        acc = acc + math.atan2(s, c) + math.atan(a) + math.asin(s * 0.5) + math.acos(c * 0.5)
        acc = acc + math.exp(-a * 0.1) + math.log(i) + math.log(i, 10)
        acc = acc + (i * 0.5) ^ 1.5
    end
    return acc
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
print("bench/math: true")
