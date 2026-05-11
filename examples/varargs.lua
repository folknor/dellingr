-- Test varargs (...)

-- Test 1: Basic vararg function
local sum = function(...)
    local a, b, c = ...
    return (a or 0) + (b or 0) + (c or 0)
end
local r1 = sum(1, 2, 3)
print("Test 1 - Basic varargs: " .. tostring(r1 == 6))

-- Test 2: Vararg with fewer values than variables
local f2 = function(...)
    local a, b, c = ...
    return a, b, c
end
local x, y, z = f2(10, 20)
print("Test 2 - Fewer varargs than vars: " .. tostring(x == 10 and y == 20 and z == nil))

-- Test 3: Return all varargs
local passthrough = function(...)
    return ...
end
local a, b, c = passthrough(1, 2, 3)
print("Test 3 - Return ...: " .. tostring(a == 1 and b == 2 and c == 3))

-- Test 4: Vararg after regular params
local mixed = function(first, ...)
    local rest1, rest2 = ...
    return first, rest1, rest2
end
local m1, m2, m3 = mixed(100, 200, 300)
print("Test 4 - Mixed params: " .. tostring(m1 == 100 and m2 == 200 and m3 == 300))

-- Test 5: Empty varargs
local empty = function(...)
    local a = ...
    return a
end
local e = empty()
print("Test 5 - Empty varargs: " .. tostring(e == nil))

-- Test 6: Vararg in expression (only first value used)
local first = function(...)
    local x = ... + 10
    return x
end
local f = first(5, 100, 200)
print("Test 6 - Vararg in expression: " .. tostring(f == 15))

-- Test 7: Pass varargs directly to another function
local inner = function(a, b, c)
    return a + b + c
end
local outer = function(...)
    return inner(...)
end
local n = outer(10, 20, 30)
print("Test 7 - Pass ... to function: " .. tostring(n == 60))

-- Test 8: Vararg-only function (no regular params)
local varonly = function(...)
    local a, b, c, d = ...
    return a + b + c + d
end
local v = varonly(1, 2, 3, 4)
print("Test 8 - Vararg-only: " .. tostring(v == 10))

-- Test 9: Pass varargs to print
local printall = function(...)
    print(...)
end
print("Test 9 - Pass ... to print (should show: hello world 42):")
printall("hello", "world", 42)

-- Test 10: Nested vararg passing
local level1 = function(...)
    return ...
end
local level2 = function(...)
    return level1(...)
end
local level3 = function(...)
    return level2(...)
end
local r10a, r10b, r10c = level3(100, 200, 300)
print("Test 10 - Nested vararg passing: " .. tostring(r10a == 100 and r10b == 200 and r10c == 300))

-- Test 11: Varargs with method call
local obj = {
    sum = function(self, ...)
        local a, b = ...
        return (a or 0) + (b or 0)
    end
}
local wrapper = function(...)
    return obj:sum(...)
end
local r11 = wrapper(5, 7)
print("Test 11 - Varargs with method call: " .. tostring(r11 == 12))

-- Test 12: Mixed fixed args and varargs in call
local adder = function(base, ...)
    local a, b = ...
    return base + (a or 0) + (b or 0)
end
local caller = function(...)
    return adder(100, ...)
end
local r12 = caller(10, 20)
print("Test 12 - Fixed + varargs in call: " .. tostring(r12 == 130))

-- Test 13: Collect varargs into table with {...}
local collect = function(...)
    return {...}
end
local t13 = collect(1, 2, 3, 4, 5)
print("Test 13 - {...} collect: " .. tostring(#t13 == 5 and t13[1] == 1 and t13[5] == 5))

-- Test 14: {...} with mixed types
local collect2 = function(...)
    local t = {...}
    return t[1], t[2], t[3]
end
local a14, b14, c14 = collect2("hello", 42, true)
print("Test 14 - {...} mixed types: " .. tostring(a14 == "hello" and b14 == 42 and c14 == true))

-- Test 15: {...} empty
local collect3 = function(...)
    return #{...}
end
local r15 = collect3()
print("Test 15 - {...} empty: " .. tostring(r15 == 0))

-- Test 16: select with negative index counts from the end
local function select_last_two(...)
    return select(-2, ...)
end
local r16a, r16b, r16c = select_last_two("a", "b", "c")
print("Test 16 - select negative: " .. tostring(r16a == "b" and r16b == "c" and r16c == nil))

print("All vararg tests complete!")
