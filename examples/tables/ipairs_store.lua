-- Workload: store_loop's update pass written as the game writes it, over
-- `ipairs`: `for _, u in ipairs(us) do u.x = u.x + u.speed end`, round after
-- round. The value the iterator yields is the record whose field the body
-- reads and stores; the body does nothing else.

local Unit = {}
Unit.__index = Unit

local units = {}
for i = 1, 64 do
    local u = setmetatable({}, Unit)
    u.id = i
    u.x = i * 2
    u.y = i * 3
    u.hp = 100
    u.speed = i % 7
    u.team = i % 2
    u.tag = true
    units[i] = u
end

function _bench()
    local us = units
    for round = 1, 20 do
        for _, u in ipairs(us) do
            u.x = u.x + u.speed
        end
    end
    local sum = 0
    for i = 1, 64 do
        sum = sum + us[i].x
    end
    return sum
end

for i = 1, 1000 do _bench() end
print("tables/ipairs_store: " .. tostring(_bench()) .. ": true")
