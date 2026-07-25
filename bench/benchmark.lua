-- brokkr verdict workload derived from examples/benchmark.lua, the mixed
-- composite suite. Unlike the other bench/ scripts this is not a wrapped
-- kernel: examples/benchmark.lua is a straight-line script with no _bench,
-- so this file restates its seven sections as one _bench call (~170ms),
-- accumulating a checksum instead of printing per-section results.

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

    -- 1. Numeric loop with arithmetic (1M iterations)
    local sum = 0
    for i = 1, 1000000 do
        sum = sum + i * 2 - 1
    end
    check = check + sum

    -- 2. Function calls (100K)
    sum = 0
    for i = 1, 100000 do
        sum = add(sum, i)
    end
    check = check + sum

    -- 3. Recursive fibonacci (call overhead + recursion)
    check = check + fib(28)

    -- 4. Table creation and field access (50K)
    sum = 0
    for i = 1, 50000 do
        local t = { a = i, b = i + 1, c = i + 2 }
        sum = sum + t.a + t.b + t.c
    end
    check = check + sum

    -- 5. Table iteration with pairs (500 x 1000 elements)
    sum = 0
    for _ = 1, 500 do
        for _, v in pairs(iter_table) do
            sum = sum + v
        end
    end
    check = check + sum

    -- 6. String operations (50K)
    sum = 0
    for _ = 1, 50000 do
        local x = string.sub(hello, 1, 10)
        sum = sum + #x
    end
    check = check + sum

    -- 7. Math operations (100K)
    sum = 0
    for i = 1, 100000 do
        sum = sum + math.sqrt(i) + math.sin(i / 1000)
    end
    check = check + math.floor(sum)

    return check
end

for i = 1, 30 do _bench() end
print("bench/benchmark: true")
