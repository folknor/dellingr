-- Benchmark suite - compatible with Lua 5.2, 5.4, and our VM
-- Run with external timing: time lua examples/benchmark.lua

-- 1. Numeric loop with arithmetic (1M iterations)
local sum = 0
for i = 1, 1000000 do
    sum = sum + i * 2 - 1
end
print("sum1: " .. tostring(sum))

-- 2. Function calls (100K)
local function add(a, b)
    return a + b
end
sum = 0
for i = 1, 100000 do
    sum = add(sum, i)
end
print("sum2: " .. tostring(sum))

-- 3. Recursive fibonacci (tests call overhead + recursion)
local function fib(n)
    if n < 2 then return n end
    return fib(n-1) + fib(n-2)
end
print("fib(28): " .. tostring(fib(28)))

-- 4. Table creation and field access (50K)
sum = 0
for i = 1, 50000 do
    local t = {a = i, b = i+1, c = i+2}
    sum = sum + t.a + t.b + t.c
end
print("sum3: " .. tostring(sum))

-- 5. Table iteration with pairs (500 iterations over 1000 elements)
local t = {}
for i = 1, 1000 do t[i] = i end
sum = 0
for j = 1, 500 do
    for k, v in pairs(t) do
        sum = sum + v
    end
end
print("sum4: " .. tostring(sum))

-- 6. String operations (50K)
local s = "hello world this is a test"
sum = 0
for i = 1, 50000 do
    local x = string.sub(s, 1, 10)
    sum = sum + #x
end
print("sum5: " .. tostring(sum))

-- 7. Math operations (100K)
sum = 0
for i = 1, 100000 do
    sum = sum + math.sqrt(i) + math.sin(i/1000)
end
print("sum6: " .. tostring(math.floor(sum)))

print("done")
