-- Probe: Lua calls of standard-library leaf functions (math.floor, math.abs,
-- math.sqrt, math.max, type, rawequal, rawget, tonumber, math.random) through
-- the library table and through local aliases, the path the dispatch loop
-- runs without entering a Rust callback.

local floor = math.floor
local abs = math.abs

function _bench()
    local t = { 1, 2, 3 }
    local sum = 0
    for i = 1, 400 do
        local x = i * 0.37
        sum = sum + floor(x) + math.floor(x + 0.5) + abs(-x) + math.sqrt(i)
        sum = sum + math.max(x, 7) + math.random(1)
        if type(x) == "number" and rawequal(t, t) then
            sum = sum + rawget(t, 2) + tonumber(i)
        end
    end
    return floor(sum)
end

for i = 1, 300 do _bench() end
print("calls/library_leaves: " .. tostring(_bench()) .. ": true")
