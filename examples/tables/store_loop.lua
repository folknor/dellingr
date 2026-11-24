-- Workload: object_fields' update pass on its own. Units are built once,
-- the object_fields way (empty table under a class metatable, then one store
-- per field, growing into map storage); the kernel then runs
-- `u.x = u.x + u.speed` over all of them, round after round. The loop body
-- stores into a receiver it reads from the array on every iteration, and
-- does nothing else.

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
        for i = 1, 64 do
            local u = us[i]
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
print("tables/store_loop: " .. tostring(_bench()) .. ": true")
