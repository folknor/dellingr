-- brokkr verdict workload derived from examples/tables/fill.lua (see that
-- file for the probe rationale). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds
-- in a release build.

local keys = {}
for i = 1, 3000 do
    keys[i] = "k_" .. i
end

local function kernel()
    local arr = {}
    for i = 1, 8000 do
        arr[i] = i
    end

    local hash = {}
    for i = 1, 3000 do
        hash[keys[i]] = i * 3
    end

    return arr[8000] + hash[keys[3000]]
end

local K = 125

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/fill: true")
