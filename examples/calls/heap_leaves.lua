-- Probe: Lua calls of the heap-writing and open-result library leaves
-- (rawset, select, table.insert, table.remove, table.pack) on their raw
-- paths, through the library table and through local aliases.

local insert = table.insert
local remove = table.remove

local function count(...)
    local n = 0
    for i = 1, select('#', ...) do
        n = n + (select(i, ...))
    end
    return n
end

function _bench()
    local t = {}
    local sum = 0
    for i = 1, 200 do
        insert(t, i)
        table.insert(t, i + 1)
        rawset(t, "k", i)
        sum = sum + remove(t) + table.remove(t, 1) + t.k
        sum = sum + count(i, 1, 2) + table.pack(i, i).n
    end
    return sum + #t
end

for i = 1, 300 do _bench() end
print("calls/heap_leaves: " .. tostring(_bench()) .. ": true")
