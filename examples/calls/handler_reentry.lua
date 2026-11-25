-- Probe: a tiny `__index` function handler and a Lua generic-for iterator,
-- each a call made from Rust that runs a nested invocation. Paired with
-- calls/handler_plain, which makes the same calls as plain Lua-to-Lua calls,
-- so the difference bounds what running them in the calling loop could save.
local function handler(_, k) return k end
local indexed = setmetatable({}, {__index = handler})
local function iterator(_, n) if n < 3 then return n + 1 end end
function _bench()
    local sum = 0
    for i = 1, 100 do
        sum = sum + indexed[i] + indexed[i + 1]
        for n in iterator, nil, 0 do sum = sum + n end
    end
    return sum
end
for i = 1, 30 do _bench() end
print("calls/handler_reentry: " .. tostring(_bench()) .. ": true")
