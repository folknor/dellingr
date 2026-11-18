-- brokkr verdict workload derived from examples/numerics/cos.lua (see that
-- file for what it tracks). Same kernel, repeated K times per _bench call so
-- the standalone run takes a few seconds in a release build.

local function kernel()
    local acc = 0
    for i = 1, 2000 do
        acc = acc + math.cos(i * 0.00314159) + math.cos(i * -0.0271828)
    end
    return acc
end

local K = 100

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/cos: true")
