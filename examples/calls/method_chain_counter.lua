-- Probe: method dispatch through a 2-level __index chain while a class
-- table on the route is written every call. Sub inherits from Base, which
-- holds the called method; the method also bumps a static counter kept on
-- Sub, the instance's metatable and so a table the inherited route reads.
-- Compare with calls/method_chain.lua, whose loop writes only the
-- instance.

local Base = {}
Base.__index = Base

local Sub = setmetatable({}, { __index = Base })
Sub.__index = Sub
Sub.calls = 0

function Base:inc(n)
    Sub.calls = Sub.calls + 1
    self.count = self.count + n
    return self.count
end

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
print("calls/method_chain_counter: " .. tostring(_bench()) .. ": true")
