-- Probe: short-lived table allocation in a hot loop. Each iteration creates a
-- fresh 4-entry table and discards it, driving allocation and garbage collection.

function _bench()
    local sum = 0
    for i = 1, 500 do
        local t = { a = i, b = i + 1, c = i + 2, d = i + 3 }
        sum = sum + t.a + t.b + t.c + t.d
    end
    return sum
end

for i = 1, 1200 do _bench() end
print("alloc/short_tables: " .. tostring(_bench()) .. ": true")
