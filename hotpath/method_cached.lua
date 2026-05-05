-- Sibling of method_dispatch.lua: caches the bound function once and
-- calls it as a plain function. Removes per-iteration field lookup and
-- the colon-method dispatch.

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
