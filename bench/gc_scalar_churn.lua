-- Wall workload paired with examples/alloc/gc_scalar_churn.lua.

local retained = {}
for i = 1, 1000000 do
    retained[i] = i
end

local function kernel()
    local sink = retained[1000000]
    for i = 1, 2048 do
        local tmp = { i, i + 1 }
        sink = sink + tmp[2]
    end
    return sink
end

local K = 24

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/gc_scalar_churn: true")
