-- Workload: a per-entity tick method that updates its receiver's fields
-- behind a guard. `Body:step` is `if self.alive then self.x = self.x + ...`:
-- the field updates sit in a method that branches, so it is no leaf and
-- every call takes an activation and dispatches each update's read, add and
-- store. The caller walks an array of bodies, so the receiver changes on
-- every call and the loop runs canonically. Three bodies in four are alive.

local Body = {}
Body.__index = Body

function Body:step(dt)
    if self.alive then
        self.x = self.x + self.vx * dt
        self.y = self.y + self.vy * dt
        self.age = self.age + 1
    end
end

local N = 16
local bodies = {}
for i = 1, N do
    bodies[i] = setmetatable({
        alive = i % 4 ~= 0,
        x = i,
        y = -i,
        vx = i % 3,
        vy = 1 - i % 5,
        age = 0,
    }, Body)
end

function _bench()
    local bs = bodies
    for round = 1, 40 do
        for i = 1, 16 do
            bs[i]:step(2)
        end
    end
    local sum = 0
    for i = 1, 16 do
        local b = bs[i]
        sum = sum + b.x + b.y + b.age
    end
    return sum
end

for i = 1, 1000 do _bench() end
print("calls/method_step: " .. tostring(_bench()) .. ": true")
