-- Tight numeric kernel.
-- Exercises opcode dispatch, number boxing, and recursive call frame setup.
-- No allocation, no string interning, no GC pressure.

local function fib(n)
    if n < 2 then return n end
    return fib(n - 1) + fib(n - 2)
end

function _bench()
    local sum = 0.0
    for i = 1, 50000 do
        sum = sum + i * 2.5 - i / 3.0
    end
    return sum + fib(20)
end

-- Standalone: iterate enough for hyperfine resolution.
for i = 1, 30 do _bench() end
print("arithmetic: true")
