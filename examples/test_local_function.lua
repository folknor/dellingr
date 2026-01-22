-- Test local function syntax

local function add(a, b)
    return a + b
end

print(add(2, 3))

-- Test recursion (the main reason for local function)
local function fib(n)
    if n < 2 then return n end
    return fib(n-1) + fib(n-2)
end

print(fib(10))
