-- brokkr verdict workload derived from examples/calls/method_step.lua (see
-- that file for what it measures). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds in a
-- release build.

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

local function kernel()
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

local K = 1000

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/method_step: true")
