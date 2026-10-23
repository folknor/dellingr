-- brokkr verdict workload derived from examples/calls/method_chain.lua
-- (see that file for the probe rationale). Same kernel, repeated K times
-- per _bench call so one call is ~100ms and the standalone run takes a
-- few seconds in a release build. Every s:inc misses on the instance and
-- on Sub, and resolves through Sub's metatable to Base.

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

local function kernel()
    for i = 1, 1000 do
        s:inc(1)
    end
    return s.count
end

local K = 560

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/method_chain: true")
