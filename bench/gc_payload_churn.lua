-- Wall workload paired with examples/alloc/gc_payload_churn.lua.

local retained = {}
for i = 1, 1000000 do
    retained[i] = i
end

local function kernel()
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

local K = 12

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/gc_payload_churn: true")
