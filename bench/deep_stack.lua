-- brokkr verdict workload derived from examples/tables/deep_stack.lua (see
-- that file for the probe rationale: the length operator on a stack whose
-- popped slots stay behind as a nil tail). Same kernel, repeated K times per
-- _bench call so one call is ~100ms and the standalone run takes a few
-- seconds in a release build.

local s = {}
for i = 1, 1024 do s[#s + 1] = i end
while #s > 500 do s[#s] = nil end

local function kernel()
    local sum = 0
    for _ = 1, 8 do
        for i = 1, 32 do
            s[#s + 1] = i
        end
        for _ = 1, 32 do
            sum = sum + s[#s]
            s[#s] = nil
        end
    end
    return sum + #s
end

local K = 400

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/deep_stack: true")
