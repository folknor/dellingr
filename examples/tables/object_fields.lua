-- Workload: objects built by assigning fields after setmetatable.
-- Each object starts empty under a class metatable that has `__index` and
-- no `__newindex`, then takes its fields one store at a time, the usual Lua
-- constructor shape: every store is a new key on a receiver with a
-- metatable, by literal name and by computed key, and the object grows from
-- inline storage into a map. An update pass then writes existing fields
-- through sites that see a different object every time.

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

function _bench()
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

for i = 1, 60 do _bench() end
print("tables/object_fields: " .. tostring(_bench()) .. ": true")
