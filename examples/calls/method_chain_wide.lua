-- Probe: method dispatch through a 2-level __index chain of classes wide
-- enough to use map storage. Sub inherits from Base, which holds the
-- called method; both carry several more methods, as real classes do, so
-- every keyed lookup on them is a hash probe rather than the short scan
-- of a small table. Compare with calls/method_chain.lua, whose classes
-- hold two keys each.

local Base = {}
Base.__index = Base

function Base:inc(n)
    self.count = self.count + n
    return self.count
end
function Base:reset() self.count = 0 end
function Base:get() return self.count end
function Base:add(o) return self.count + o.count end
function Base:is_zero() return self.count == 0 end
function Base:describe() return "base" end

local Sub = setmetatable({}, { __index = Base })
Sub.__index = Sub

function Sub.new()
    local s = setmetatable({}, Sub)
    s.count = 0
    return s
end
function Sub:describe() return "sub" end
function Sub:twice() return self.count * 2 end
function Sub:half() return self.count / 2 end
function Sub:negate() return -self.count end

local s = Sub.new()

function _bench()
    for i = 1, 1000 do
        s:inc(1)
    end
    return s.count
end

for i = 1, 400 do _bench() end
print("calls/method_chain_wide: " .. tostring(_bench()) .. ": true")
