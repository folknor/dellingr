-- brokkr verdict workload derived from examples/fields/polymorphic.lua (see
-- that file for what it measures). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds in a
-- release build.

local items = {}
for i = 1, 5000 do
    items[i] = { id = i, value = i * 2 }
end

local function kernel()
    local sum = 0
    for i = 1, 5000 do
        local entry = items[i]
        sum = sum + entry.id + entry.value
    end
    return sum
end

local K = 900

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/polymorphic: true")
