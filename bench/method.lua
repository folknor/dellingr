-- brokkr verdict workload derived from examples/calls/method.lua
-- (see that file for the probe rationale). Same kernel, repeated K times
-- per _bench call so one call is ~100ms and the standalone run takes a
-- few seconds in a release build. Every c:inc misses on the instance and
-- resolves through the metatable's __index.

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

local function kernel()
    for i = 1, 1000 do
        c:inc(1)
    end
    return c.count
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
print("bench/method: true")
