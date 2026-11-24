-- brokkr verdict workload derived from examples/alloc/short_tables.lua (see
-- that file for what it measures). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds in a
-- release build.

local function kernel()
    local sum = 0
    for i = 1, 500 do
        local t = { a = i, b = i + 1, c = i + 2, d = i + 3 }
        sum = sum + t.a + t.b + t.c + t.d
    end
    return sum
end

local K = 1500

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/short_tables: true")
