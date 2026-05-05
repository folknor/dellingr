-- Probe: method dispatch through a 2-level __index chain.
-- Sub inherits from Base, which holds the actual methods. Each
-- `obj:inc(1)` walks Sub.__index -> Base.__index -> method. Compare with
-- calls/method.lua (single-level chain) to measure how much extra
-- the deeper walk costs.

local Base = {}
Base.__index = Base

function Base:inc(n)
    self.count = self.count + n
    return self.count
end

local Sub = setmetatable({}, { __index = Base })
Sub.__index = Sub

function Sub.new()
    local s = setmetatable({}, Sub)
    s.count = 0
    return s
end

local s = Sub.new()

function _bench()
    for i = 1, 1000 do
        s:inc(1)
    end
    return s.count
end

for i = 1, 400 do _bench() end
print("calls/method_chain: true")
