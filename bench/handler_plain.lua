-- The calls of handler_reentry made as plain Lua-to-Lua calls, in the same
-- proportions in both workload files.
local function handler(_, k) return k end
local indexed = {}
local function iterator(_, n) if n < 3 then return n + 1 end end
local function kernel()
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
function _bench()
    local sum = 0
    for i = 1, 1500 do sum = sum + kernel() end
    return sum
end
for i = 1, 30 do _bench() end
print("bench/handler_plain: true")
