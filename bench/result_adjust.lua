-- brokkr verdict workload derived from examples/calls/result_adjust.lua (see
-- that file for the probe rationale: padded and cut call results, and
-- surplus arguments dropped at admission). Same kernel, repeated K times per
-- _bench call so one call is ~100ms and the standalone run takes a few
-- seconds in a release build.

local function one(x)
    return x
end

local function three(x)
    return x, x + 1, x + 2
end

local function two_params(a, b)
    return a + b
end

local floor = math.floor

local function kernel()
    local sum = 0
    for i = 1, 400 do
        local a, b, c = one(i)
        local d = three(i)
        local e = two_params(i, 1, 2, 3, 4)
        local f, g, h = floor(i + 0.5)
        if b == nil and c == nil and g == nil and h == nil then
            sum = sum + a + d + e + f
        end
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
print("bench/result_adjust: true")
