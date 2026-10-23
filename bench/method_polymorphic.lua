-- brokkr verdict workload derived from examples/calls/method_polymorphic.lua
-- (see that file for the probe rationale). Same kernel, repeated K times
-- per _bench call so one call is ~100ms and the standalone run takes a
-- few seconds in a release build.

local Shape = {}
Shape.__index = Shape

function Shape:area()
    return self.w * self.h
end

-- Inherits area from Shape: two levels up from an instance.
local Rect = setmetatable({}, { __index = Shape })
Rect.__index = Rect

local Circle = {}
Circle.__index = Circle

function Circle:area()
    return 3 * self.r * self.r
end

local Square = {}
Square.__index = Square

function Square:area()
    return self.s * self.s
end

local shapes = {}
for i = 1, 8 do
    if i % 4 == 0 then
        shapes[i] = setmetatable({ w = i, h = 2 }, Rect)
    elseif i % 2 == 0 then
        shapes[i] = setmetatable({ r = i }, Circle)
    else
        shapes[i] = setmetatable({ s = i }, Square)
    end
end

local function kernel()
    local total = 0
    for _ = 1, 125 do
        for i = 1, 8 do
            total = total + shapes[i]:area()
        end
    end
    return total
end

local K = 700

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/method_polymorphic: true")
