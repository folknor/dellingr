-- Probe: proper tail calls (`return f(...)`). A tail-recursive count, a
-- two-state machine whose states hand off by tail call, and a Lua function
-- whose tail callee is a Rust function. The depth stays below the call-depth
-- limit so a VM without tail calls runs the same kernel for an A/B.

local function count_down(n, acc)
    if n == 0 then
        return acc
    end
    return count_down(n - 1, acc + 1)
end

local state_a, state_b
function state_a(n, acc)
    if n == 0 then
        return acc
    end
    return state_b(n - 1, acc + 2)
end
function state_b(n, acc)
    if n == 0 then
        return acc
    end
    return state_a(n - 1, acc - 1)
end

local function floor_of(x)
    return math.floor(x)
end

function _bench()
    local sum = 0
    for i = 1, 20 do
        sum = sum + count_down(50, 0) + state_a(50, 0) + floor_of(i + 0.5)
    end
    return sum
end

for i = 1, 300 do _bench() end
print("calls/tail_call: " .. tostring(_bench()) .. ": true")
