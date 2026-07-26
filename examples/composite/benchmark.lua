-- Mixed composite kernel: the seven sections of bench/benchmark.lua at
-- ~1/100 scale, so one _bench call is instrumentation-safe (~2ms, not
-- ~170ms). Registered as the benchmark workload's hotpath_file in
-- brokkr.toml - the instrumented modes (--hotpath / --alloc) resolve this
-- file, --bench resolves the seconds-scale bench/ port. Same kernels in
-- the same proportions, so per-function percentages carry over.

local function fib(n)
    if n < 2 then return n end
    return fib(n - 1) + fib(n - 2)
end

local function add(a, b)
    return a + b
end

local iter_table = {}
for i = 1, 1000 do iter_table[i] = i end

local hello = "hello world this is a test"

function _bench()
    local check = 0

    -- 1. Numeric loop with arithmetic
    local sum = 0
    for i = 1, 10000 do
        sum = sum + i * 2 - 1
    end
    check = check + sum

    -- 2. Function calls
    sum = 0
    for i = 1, 1000 do
        sum = add(sum, i)
    end
    check = check + sum

    -- 3. Recursive fibonacci (call overhead + recursion)
    check = check + fib(19)

    -- 4. Table creation and field access
    sum = 0
    for i = 1, 500 do
        local t = { a = i, b = i + 1, c = i + 2 }
        sum = sum + t.a + t.b + t.c
    end
    check = check + sum

    -- 5. Table iteration with pairs (5 x 1000 elements)
    sum = 0
    for _ = 1, 5 do
        for _, v in pairs(iter_table) do
            sum = sum + v
        end
    end
    check = check + sum

    -- 6. String operations
    sum = 0
    for _ = 1, 500 do
        local x = string.sub(hello, 1, 10)
        sum = sum + #x
    end
    check = check + sum

    -- 7. Math operations
    sum = 0
    for i = 1, 1000 do
        sum = sum + math.sqrt(i) + math.sin(i / 1000)
    end
    check = check + math.floor(sum)

    return check
end

-- Standalone: iterate enough for hyperfine resolution.
for i = 1, 30 do _bench() end
print("composite/benchmark: true")
