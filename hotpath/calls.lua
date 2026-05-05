-- Function-call kernel.
-- Exercises Lua closure calls, stack frame setup/teardown, and return balancing
-- with minimal allocation and table traffic.

local function inc(x)
    return x + 1
end

local function mix(x)
    return inc(x) + inc(x + 1) + inc(x + 2)
end

function _bench()
    local sum = 0
    for i = 1, 10000 do
        sum = sum + mix(i)
    end
    return sum
end
