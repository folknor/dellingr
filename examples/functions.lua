-- Test multiple return values

-- Test 1: Empty return
local f1 = function() return end
local r1 = f1()
print("Test 1 - Empty return: " .. tostring(r1 == nil))

-- Test 2: Single return
local f2 = function() return 42 end
local r2 = f2()
print("Test 2 - Single return: " .. tostring(r2 == 42))

-- Test 3: Multiple return values
local f3 = function() return 1, 2, 3 end
local a, b, c = f3()
print("Test 3 - Multiple returns: " .. tostring(a == 1 and b == 2 and c == 3))

-- Test 4: More variables than return values (extras get nil)
local f4 = function() return 10, 20 end
local x, y, z = f4()
print("Test 4 - Fewer returns than vars: " .. tostring(x == 10 and y == 20 and z == nil))

-- Test 5: Fewer variables than return values (extras discarded)
local f5 = function() return 100, 200, 300 end
local p, q = f5()
print("Test 5 - More returns than vars: " .. tostring(p == 100 and q == 200))

-- Test 6: Return values used in expression
local f6 = function() return 5, 10 end
local sum = f6() + 1  -- Only first return value used
print("Test 6 - Return in expression: " .. tostring(sum == 6))

-- Test 7: Nested function calls with multiple returns
local inner = function() return 1, 2 end
local outer = function() return inner(), 3 end
local i1, i2, i3 = outer()
-- Note: inner() in middle position only contributes first value
print("Test 7 - Nested returns: " .. tostring(i1 == 1 and i2 == 3 and i3 == nil))

-- Test 8: Return with expression
local f8 = function(x) return x * 2, x * 3 end
local d, t = f8(5)
print("Test 8 - Return expressions: " .. tostring(d == 10 and t == 15))

-- Test 9: Chained returns
local chain1 = function() return 1, 2, 3 end
local chain2 = function() return chain1() end
local c1, c2, c3 = chain2()
print("Test 9 - Chained returns: " .. tostring(c1 == 1 and c2 == 2 and c3 == 3))

-- Test 10: Practical example - swap function
local swap = function(a, b) return b, a end
local s1, s2 = swap(10, 20)
print("Test 10 - Swap: " .. tostring(s1 == 20 and s2 == 10))

-- Test 11: Multiple returns as function arguments (last position)
local add = function(a, b) return a + b end
local getValues = function() return 3, 4 end
local r11 = add(getValues())
print("Test 11 - Multi-return as args: " .. tostring(r11 == 7))

-- Test 12: Multiple returns in middle position (only first used)
local threeArgs = function(a, b, c) return a + b + c end
local twoVals = function() return 10, 20 end
local r12 = threeArgs(twoVals(), 5, 100)  -- twoVals() in middle, only first value (10) used
print("Test 12 - Multi-return in middle: " .. tostring(r12 == 115))  -- 10 + 5 + 100 = 115

-- Test 13: Nested multi-return as arguments
local identity = function(x, y) return x, y end
local r13 = add(identity(5, 10))  -- identity returns 5, 10; add gets both
print("Test 13 - Nested multi-return args: " .. tostring(r13 == 15))

print("All function tests complete!")
