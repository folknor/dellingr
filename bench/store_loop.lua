-- brokkr verdict workload derived from examples/tables/store_loop.lua (see
-- that file for what it measures). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds in a
-- release build.

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

local function kernel()
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

local K = 1000

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/store_loop: true")
