-- brokkr verdict workload derived from examples/calls/tail_call.lua (see
-- that file for the probe rationale: tail-recursive calls, a state machine
-- handing off by tail call, and a Rust tail callee). Same kernel, repeated K
-- times per _bench call so one call is ~100ms and the standalone run takes a
-- few seconds in a release build.

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

local function kernel()
    local sum = 0
    for i = 1, 20 do
        sum = sum + count_down(50, 0) + state_a(50, 0) + floor_of(i + 0.5)
    end
    return sum
end

local K = 1000

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/tail_call: true")
