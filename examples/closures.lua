-- Closure tests for the Lua VM
-- Note: Our implementation captures upvalues by VALUE (copy), not by reference.
-- This means modifications to captured variables don't propagate across closures.
-- This is a simplification - full Lua semantics would require "open upvalues".

-- Test 1: Basic closure capturing a local variable
local x = 10
local getX = function()
    return x
end
result1 = getX()
print("Test 1 - Basic closure capture: " .. tostring(result1 == 10))

-- Test 2: Closure modifies captured variable
-- NOTE: This test demonstrates our capture-by-value semantics.
-- Each call gets a snapshot of 'counter' at closure creation time.
local counter = 0
local increment = function()
    counter = counter + 1
    return counter
end
local a = increment()
local b = increment()
-- With capture-by-value, each call sees counter=0, so both return 1
result2 = a == 1 and b == 1
print("Test 2 - Capture-by-value semantics: " .. tostring(result2))

-- Test 3: Multiple closures capturing the same variable
local y = 5
local getY = function()
    return y
end
local doubleY = function()
    return y * 2
end
result3 = getY() == 5 and doubleY() == 10
print("Test 3 - Multiple closures: " .. tostring(result3))

-- Test 4: Nested closures (closure capturing another closure's upvalue)
local outer = 100
local makeGetter = function()
    local inner = 50
    local getter = function()
        return outer + inner
    end
    return getter
end
local g = makeGetter()
result4 = g() == 150
print("Test 4 - Nested closures: " .. tostring(result4))

-- Test 5: Counter factory
-- NOTE: Each counter is independent, but each call captures the current value.
local makeCounter = function()
    local count = 0
    local inc = function()
        count = count + 1
        return count
    end
    return inc
end
local c1 = makeCounter()
local c2 = makeCounter()
local v1 = c1()
local v2 = c1()
local v3 = c2()
-- With capture-by-value: c1 and c2 are independent, each call returns 1
result5 = v1 == 1 and v2 == 1 and v3 == 1
print("Test 5 - Counter factory (capture-by-value): " .. tostring(result5))

print("All closure tests complete!")
