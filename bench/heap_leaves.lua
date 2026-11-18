-- brokkr verdict workload derived from examples/calls/heap_leaves.lua (see
-- that file for the probe rationale: Lua calls of the heap-writing and
-- open-result library leaves). Same kernel, repeated K times per _bench call
-- so one call is ~100ms and the standalone run takes a few seconds in a
-- release build.

local insert = table.insert
local remove = table.remove

local function count(...)
    local n = 0
    for i = 1, select('#', ...) do
        n = n + (select(i, ...))
    end
    return n
end

local function kernel()
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

local K = 1000

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/heap_leaves: true")
