-- Probe: the calls of calls/handler_reentry (an `__index` handler and a
-- generic-for iterator) made as plain Lua-to-Lua calls in the same loop
-- shape, the control for what the nested invocation costs.
local function handler(_, k) return k end
local indexed = {}
local function iterator(_, n) if n < 3 then return n + 1 end end
function _bench()
    local sum = 0
    for i = 1, 100 do
        sum = sum + handler(indexed, i) + handler(indexed, i + 1)
        local n = iterator(nil, 0)
        while n do
            sum = sum + n
            n = iterator(nil, n)
        end
    end
    return sum
end
for i = 1, 30 do _bench() end
print("calls/handler_plain: " .. tostring(_bench()) .. ": true")
