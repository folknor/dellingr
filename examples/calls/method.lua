-- Probe: obj:method() desugars to a method lookup + call. Every c:inc
-- misses on the instance and resolves through the metatable's __index.
-- Compare with a cached-method version (calls/method_cached.lua).

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

function _bench()
    for i = 1, 1000 do
        c:inc(1)
    end
    return c.count
end

for i = 1, 500 do _bench() end
print("calls/method: " .. tostring(_bench()) .. ": true")
