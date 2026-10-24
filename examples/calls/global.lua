-- Probe: calling a user-defined global through the global table read cache.
-- A warm GET_GLOBAL validates the table version and reads its cached slot.
-- Compare against calls/local.lua, which caches the same function in a local.

function add_one(x)
    return x + 1
end

function _bench()
    local sum = 0
    for i = 1, 1000 do
        sum = add_one(sum)
    end
    return sum
end

for i = 1, 1500 do _bench() end
print("calls/global: " .. tostring(_bench()) .. ": true")
