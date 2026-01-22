-- Closure tests for the Lua VM
-- Our implementation properly captures upvalues by REFERENCE using open/closed upvalues.
-- This means modifications to captured variables propagate correctly.

-- Test 1: Basic closure capturing a local variable
local x = 10
local getX = function()
    return x
end
result1 = getX()
print("Test 1 - Basic closure capture: " .. tostring(result1 == 10))

-- Test 2: Closure modifies captured variable
-- With proper capture-by-reference, each increment actually modifies the counter.
local counter = 0
local increment = function()
    counter = counter + 1
    return counter
end
local a = increment()
local b = increment()
-- With capture-by-reference, a == 1 and b == 2
result2 = a == 1 and b == 2
print("Test 2 - Capture-by-reference semantics: " .. tostring(result2))

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
-- With proper capture-by-reference, each counter maintains its own state.
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
-- c1 returns 1, then 2; c2 returns 1 (independent counter)
result5 = v1 == 1 and v2 == 2 and v3 == 1
print("Test 5 - Counter factory: " .. tostring(result5))

-- Test 6: Two closures sharing the same upvalue
local makeCounter2 = function()
    local count = 0
    local inc = function()
        count = count + 1
        return count
    end
    local get = function()
        return count
    end
    return inc, get
end
local inc, get = makeCounter2()
local x1 = inc()  -- count becomes 1
local x2 = get()  -- should see count == 1
local x3 = inc()  -- count becomes 2
local x4 = get()  -- should see count == 2
result6 = x1 == 1 and x2 == 1 and x3 == 2 and x4 == 2
print("Test 6 - Shared upvalue between closures: " .. tostring(result6))

print("All closure tests complete!")
