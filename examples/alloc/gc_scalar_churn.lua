-- Workload: small tables churn beside a live million-slot scalar array.

local retained = {}
for i = 1, 1000000 do
    retained[i] = i
end

function _bench()
    local sink = retained[1000000]
    for i = 1, 2048 do
        local tmp = { i, i + 1 }
        sink = sink + tmp[2]
    end
    return sink
end

print("alloc/gc_scalar_churn: " .. tostring(_bench()) .. ": true")
