-- A tiny `__index` function handler and a Lua generic-for iterator, in the
-- same proportions in both workload files.
local function handler(_, k) return k end
local indexed = setmetatable({}, {__index = handler})
local function iterator(_, n) if n < 3 then return n + 1 end end
local function kernel()
    local sum = 0
    for i = 1, 100 do
        sum = sum + indexed[i] + indexed[i + 1]
        for n in iterator, nil, 0 do sum = sum + n end
    end
    return sum
end
function _bench()
    local sum = 0
    for i = 1, 1500 do sum = sum + kernel() end
    return sum
end
for i = 1, 30 do _bench() end
print("bench/handler_reentry: true")
