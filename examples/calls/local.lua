-- Sibling of calls/global.lua: same loop, but the function is bound to a
-- local up front so each call skips the globals lookup. The delta vs
-- calls/global.lua is the cost of per-call global resolution.

local function add_one(x)
    return x + 1
end

function _bench()
    local sum = 0
    local f = add_one
    for i = 1, 1000 do
        sum = f(sum)
    end
    return sum
end

for i = 1, 1500 do _bench() end
print("calls/local: true")
