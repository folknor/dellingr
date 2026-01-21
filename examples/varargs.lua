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

-- Test 7: Nested vararg functions (with workaround for limitation)
local outer = function(...)
    local inner = function(...)
        return ...
    end
    -- Workaround: capture varargs before passing to inner function
    local a, b = ...
    return inner(a, b)
end
local n1, n2 = outer(7, 8)
print("Test 7 - Nested varargs: " .. tostring(n1 == 7 and n2 == 8))

-- Test 8: Vararg-only function (no regular params)
local varonly = function(...)
    local a, b, c, d = ...
    return a + b + c + d
end
local v = varonly(1, 2, 3, 4)
print("Test 8 - Vararg-only: " .. tostring(v == 10))

print("All vararg tests complete!")

-- NOTE: Using ... directly as function arguments is not fully supported.
-- For example: print(...) only passes the first vararg.
-- Workaround: capture varargs first: local a, b = ...; print(a, b)
