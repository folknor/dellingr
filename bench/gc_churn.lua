-- brokkr verdict workload derived from examples/alloc/gc_churn.lua (see
-- that file for the probe rationale: sustained live heap + per-iteration
-- garbage, so collections have real marking work). Same kernel, repeated
-- K times per _bench call so one call is ~100ms and the standalone run
-- takes a few seconds in a release build.

local retained = {}
for i = 1, 200 do
    retained[i] = {
        id = i,
        tag = "node",
        kids = { i, i + 1, i + 2 },
    }
end

local function kernel()
    local sink = 0
    for i = 1, 200 do
        local tmp = { a = i, b = i + 1 }
        local n = i
        local f = function()
            return n + tmp.a
        end
        sink = sink + f()
    end
    return sink
end

local K = 1600

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/gc_churn: true")
