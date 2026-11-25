-- Workload: store_loop's update pass behind a truth test of a record
-- field, the game shape `if u.alive then u.x = u.x + u.speed end`. Units
-- are built the store_loop way; every other one is alive, so both edges of
-- the test run on every round.

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
    u.alive = i % 2 == 0
    units[i] = u
end

function _bench()
    local us = units
    for round = 1, 20 do
        for i = 1, 64 do
            local u = us[i]
            if u.alive then
                u.x = u.x + u.speed
            end
        end
    end
    local sum = 0
    for i = 1, 64 do
        sum = sum + us[i].x
    end
    return sum
end

for i = 1, 1000 do _bench() end
print("tables/alive_update: " .. tostring(_bench()) .. ": true")
