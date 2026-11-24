-- brokkr verdict workload derived from examples/iter/ipairs.lua (see
-- that file for what it measures). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds in a
-- release build.

local arr = {}
for i = 1, 10000 do
    arr[i] = i
end

local function kernel()
    local sum = 0
    for _, v in ipairs(arr) do
        sum = sum + v
    end
    return sum
end

local K = 530

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/ipairs: true")
