-- Workload: delete and re-add churn on a string-keyed dictionary.
-- Every key of a map-storage table is cleared and set again, so each
-- write re-adds a tombstoned key: the old slot turns vacant, the key
-- moves to the back of pairs order, and compaction runs once the vacant
-- slots reach the live count. Registries, sets and job tables churn this way.

local keys = {}
for i = 1, 64 do
    keys[i] = "job_" .. i
end

function _bench()
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

for i = 1, 60 do _bench() end
print("tables/rekey: " .. tostring(_bench()) .. ": true")
