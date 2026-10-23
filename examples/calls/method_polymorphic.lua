-- Probe: one method-call site serving several classes, most answering the
-- method one __index level up and one inheriting it from a base class two
-- levels up. Every one-level receiver should take the register fast path
-- whatever the site resolved for the two-level one, so this measures how
-- much the shared site costs the one-level classes. Compare with
-- calls/method.lua (one class, one level) and calls/method_chain.lua (one
-- class, two levels).

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

function _bench()
    local total = 0
    for _ = 1, 125 do
        for i = 1, 8 do
            total = total + shapes[i]:area()
        end
    end
    return total
end

for i = 1, 300 do _bench() end
print("calls/method_polymorphic: true")
