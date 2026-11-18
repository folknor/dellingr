-- brokkr verdict workload derived from examples/tables/object_fields.lua
-- (see that file for the probe rationale). Same kernel, repeated K times per
-- _bench call so one call is ~100ms and the standalone run takes a few
-- seconds in a release build.

local Unit = {}
Unit.__index = Unit

function Unit:damage(n)
    self.hp = self.hp - n
end

local tags = {}
for i = 1, 8 do
    tags[i] = "tag_" .. i
end

local function new_unit(i)
    local u = setmetatable({}, Unit)
    u.id = i
    u.x = i * 2
    u.y = i * 3
    u.hp = 100
    u.speed = i % 7
    u.team = i % 2
    u[tags[i % 8 + 1]] = true
    return u
end

local function kernel()
    local total = 0
    for round = 1, 20 do
        local units = {}
        for i = 1, 64 do
            units[i] = new_unit(i + round)
        end
        for i = 1, 64 do
            local u = units[i]
            u:damage(i % 5)
            u.x = u.x + u.speed
            total = total + u.hp + u.x
        end
    end
    return total
end

local K = 100

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/object_fields: true")
