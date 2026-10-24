-- Probe: sustained live heap plus per-iteration garbage, so that collections
-- have real marking work to do rather than sweeping a nearly empty heap.
-- `retained` stays reachable for the whole run, so every mark pass walks it;
-- the loop body then churns short-lived tables and closures on top.
-- Exercises worklist marking of the retained graph, sweeping short-lived
-- objects, and reclaiming captured upvalue slots for reuse by later closures.
-- Compare against alloc/closure.lua, which churns closures against an almost
-- empty heap and so barely exercises marking.

local retained = {}
for i = 1, 200 do
    retained[i] = {
        id = i,
        tag = "node",
        kids = { i, i + 1, i + 2 },
    }
end

function _bench()
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

for i = 1, 400 do _bench() end
print("alloc/gc_churn: " .. tostring(_bench()) .. ": true")
