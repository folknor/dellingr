-- Workload: arrays with backing storage churn beside a live million-slot scalar array.

local retained = {}
for i = 1, 1000000 do
    retained[i] = i
end

function _bench()
    local sink = retained[1000000]
    for i = 1, 128 do
        local tmp = {}
        for j = 1, 1024 do
            tmp[j] = i + j
        end
        sink = sink + tmp[1024]
    end
    return sink
end

print("alloc/gc_payload_churn: " .. tostring(_bench()) .. ": true")
