-- Probe: cache the method function once and call it as a plain function with
-- the receiver passed explicitly, avoiding per-iteration method lookup.

local Counter = {}
Counter.__index = Counter

function Counter:inc(n)
    self.count = self.count + n
    return self.count
end

function Counter.new()
    local c = setmetatable({}, Counter)
    c.count = 0
    return c
end

local c = Counter.new()
local inc = Counter.inc

function _bench()
    for i = 1, 1000 do
        inc(c, 1)
    end
    return c.count
end

for i = 1, 700 do _bench() end
print("calls/method_cached: " .. tostring(_bench()) .. ": true")
