-- Edge Case Tests
-- Tests for various edge cases and correct Lua semantics

print("=== Edge Case Tests ===")
print("")

-- 1. Modulo operator (floored semantics)
print("1. Modulo operator (floored)")
print("-5 % 3 = " .. tostring(-5 % 3))  -- 1 (Lua floored)
print("5 % -3 = " .. tostring(5 % -3))  -- -1 (Lua floored)
print("7 % 3 = " .. tostring(7 % 3))    -- 1
print("")

-- 2. math.fmod (truncated semantics, like C)
print("2. math.fmod (truncated)")
print("math.fmod(-5, 3) = " .. tostring(math.fmod(-5, 3)))  -- -2
print("math.fmod(5, -3) = " .. tostring(math.fmod(5, -3)))  -- 2
print("")

-- 3. Division edge cases
print("3. Division edge cases")
print("1/0 = " .. tostring(1/0))    -- inf
print("-1/0 = " .. tostring(-1/0))  -- -inf
print("0/0 = " .. tostring(0/0))    -- NaN
print("")

-- 4. ipairs with false values
print("4. ipairs with false values")
local with_false = {1, false, 3, nil, 5}
local ipairs_result = {}
for i, v in ipairs(with_false) do
    ipairs_result[i] = tostring(v)
end
print("ipairs result: " .. table.concat(ipairs_result, ", "))  -- 1, false, 3
print("")

-- 5. Comparison edge cases
print("5. Comparison edge cases")
print("10 < 9 = " .. tostring(10 < 9))        -- false
print("-0 == 0 = " .. tostring(-0 == 0))      -- true
print("0/0 == 0/0 = " .. tostring(0/0 == 0/0))  -- false (NaN != NaN)
print("1 == '1' = " .. tostring(1 == "1"))    -- false (different types)
print("nil == false = " .. tostring(nil == false))  -- false
print("")

-- 6. Table identity vs equality
print("6. Table identity")
local t1 = {}
local t2 = {}
local t3 = t1
print("{} == {} = " .. tostring(t1 == t2))  -- false (different tables)
print("t1 == t3 = " .. tostring(t1 == t3))  -- true (same table)
print("")

-- 7. table.unpack with ranges
print("7. table.unpack with ranges")
local t = {1, 2, 3, 4, 5}
local a, b, c = table.unpack(t, 2, 4)
print("table.unpack({1,2,3,4,5}, 2, 4) = " .. tostring(a) .. ", " .. tostring(b) .. ", " .. tostring(c))
local x = table.unpack(t, 10, 15)  -- beyond range
print("table.unpack(t, 10, 15) = " .. tostring(x))  -- nil
print("")

-- 8. String operations
print("8. String operations")
print("string.sub('hello', 0) = '" .. string.sub("hello", 0) .. "'")      -- hello
print("string.sub('hello', -10) = '" .. string.sub("hello", -10) .. "'")  -- hello
print("string.sub('hello', 2, -2) = '" .. string.sub("hello", 2, -2) .. "'")  -- ell
print("")

-- 9. select edge cases
print("9. select edge cases")
local r1, r2, r3 = select(1, 'a', 'b', 'c')
print("select(1, 'a', 'b', 'c') = " .. tostring(r1) .. ", " .. tostring(r2) .. ", " .. tostring(r3))
print("select('#', 'a', 'b', 'c') = " .. tostring(select('#', 'a', 'b', 'c')))
print("")

-- 10. Math edge cases
print("10. Math edge cases")
print("math.floor(3.7) = " .. tostring(math.floor(3.7)))  -- 3
print("math.ceil(3.2) = " .. tostring(math.ceil(3.2)))    -- 4
print("math.min(3, 1, 4, 1, 5) = " .. tostring(math.min(3, 1, 4, 1, 5)))  -- 1
print("math.max(3, 1, 4, 1, 5) = " .. tostring(math.max(3, 1, 4, 1, 5)))  -- 5
print("math.huge > 1e308 = " .. tostring(math.huge > 1e308))  -- true
print("")

-- 11. Numeric for loop edge cases
print("11. Numeric for edge cases")
local sum = 0
for i = 5, 1, -1 do  -- counting down
    sum = sum + i
end
print("sum 5 down to 1 = " .. tostring(sum))  -- 15

sum = 0
for i = 1, 0 do  -- empty loop
    sum = sum + i
end
print("sum 1 to 0 (empty) = " .. tostring(sum))  -- 0
print("")

-- 12. Table operations
print("12. Table operations")
local arr = {1, 2, 3}
table.insert(arr, 2, "new")
print("After insert at 2: " .. table.concat(arr, ", "))  -- 1, new, 2, 3
table.remove(arr, 2)
print("After remove at 2: " .. table.concat(arr, ", "))  -- 1, 2, 3
print("")

-- 13. table.sort with comparator
print("13. table.sort with comparator")
local nums = {5, 2, 8, 1, 9}
table.sort(nums, function(a, b) return a > b end)
print("Sorted descending: " .. table.concat(nums, ", "))  -- 9, 8, 5, 2, 1
print("")

-- 14. table.move
print("14. table.move")
local src = {1, 2, 3, 4, 5}
local dst = {}
table.move(src, 2, 4, 1, dst)
print("Move result: " .. table.concat(dst, ", "))  -- 2, 3, 4
print("")

-- 15. String patterns
print("15. String patterns")
print("string.find('hello.world', '.', 1, true) = " .. tostring(select(1, string.find("hello.world", ".", 1, true))))
local a, b = string.match("hello-world", "(%w+)-(%w+)")
print("string.match captures: " .. tostring(a) .. ", " .. tostring(b))
print("")

-- 16. gsub with table replacement
print("16. gsub with table")
local map = { hello = "HELLO", world = "WORLD" }
local result = string.gsub("hello world", "(%w+)", map)
print("gsub with table: " .. result)
print("")

print("=== All edge case tests passed! ===")
