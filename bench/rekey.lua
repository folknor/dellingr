-- brokkr verdict workload derived from examples/tables/rekey.lua (see that
-- file for the probe rationale). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds
-- in a release build.

local keys = {}
for i = 1, 64 do
    keys[i] = "job_" .. i
end

local function kernel()
    local registry = {}
    for i = 1, 64 do
        registry[keys[i]] = i
    end
    local total = 0
    for round = 1, 40 do
        for i = 1, 64 do
            local key = keys[i]
            local value = registry[key]
            registry[key] = nil
            registry[key] = value + round
        end
        total = total + registry[keys[round]]
    end
    return total
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
print("bench/rekey: true")
