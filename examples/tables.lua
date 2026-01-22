-- Test table library functions

-- Test 1: table.insert at end (append)
local t1 = {1, 2, 3}
table.insert(t1, 4)
print("Test 1 - table.insert append: " .. tostring(t1[1] == 1 and t1[2] == 2 and t1[3] == 3 and t1[4] == 4))

-- Test 2: table.insert at position
local t2 = {1, 3, 4}
table.insert(t2, 2, 2)
print("Test 2 - table.insert at pos: " .. tostring(t2[1] == 1 and t2[2] == 2 and t2[3] == 3 and t2[4] == 4))

-- Test 3: table.insert at beginning
local t3 = {2, 3}
table.insert(t3, 1, 1)
print("Test 3 - table.insert at start: " .. tostring(t3[1] == 1 and t3[2] == 2 and t3[3] == 3))

-- Test 4: table.remove from end (default)
local t4 = {1, 2, 3}
local removed4 = table.remove(t4)
print("Test 4 - table.remove end: " .. tostring(removed4 == 3 and t4[1] == 1 and t4[2] == 2 and t4[3] == nil))

-- Test 5: table.remove at position
local t5 = {1, 2, 3, 4}
local removed5 = table.remove(t5, 2)
print("Test 5 - table.remove at pos: " .. tostring(removed5 == 2 and t5[1] == 1 and t5[2] == 3 and t5[3] == 4))

-- Test 6: table.remove from beginning
local t6 = {1, 2, 3}
local removed6 = table.remove(t6, 1)
print("Test 6 - table.remove at start: " .. tostring(removed6 == 1 and t6[1] == 2 and t6[2] == 3))

-- Test 7: table.sort (default ascending)
local t7 = {3, 1, 4, 1, 5, 9, 2, 6}
table.sort(t7)
print("Test 7 - table.sort default: " .. tostring(t7[1] == 1 and t7[2] == 1 and t7[3] == 2 and t7[4] == 3 and t7[5] == 4 and t7[6] == 5 and t7[7] == 6 and t7[8] == 9))

-- Test 8: table.sort with comparator (descending)
local t8 = {3, 1, 4}
table.sort(t8, function(a, b) return a > b end)
print("Test 8 - table.sort with comp: " .. tostring(t8[1] == 4 and t8[2] == 3 and t8[3] == 1))

-- Test 9: table.unpack basic
local t9 = {10, 20, 30}
local a, b, c = table.unpack(t9)
print("Test 9 - table.unpack basic: " .. tostring(a == 10 and b == 20 and c == 30))

-- Test 10: table.unpack with range
local t10 = {1, 2, 3, 4, 5}
local x, y = table.unpack(t10, 2, 3)
print("Test 10 - table.unpack range: " .. tostring(x == 2 and y == 3))

-- Test 11: empty table operations
local t11 = {}
table.insert(t11, "first")
print("Test 11 - insert into empty: " .. tostring(t11[1] == "first"))

-- Test 12: table.sort with strings
local t12 = {"banana", "apple", "cherry"}
table.sort(t12)
print("Test 12 - sort strings: " .. tostring(t12[1] == "apple" and t12[2] == "banana" and t12[3] == "cherry"))

-- Test 13: table.pack basic
local t13 = table.pack("a", "b", "c")
print("Test 13 - table.pack basic: " .. tostring(t13.n == 3 and t13[1] == "a" and t13[2] == "b" and t13[3] == "c"))

-- Test 14: table.pack empty
local t14 = table.pack()
print("Test 14 - table.pack empty: " .. tostring(t14.n == 0))

-- Test 15: table.concat basic
local t15 = {"a", "b", "c"}
local s15 = table.concat(t15, "-")
print("Test 15 - table.concat: " .. tostring(s15 == "a-b-c"))

-- Test 16: table.concat with range
local t16 = {1, 2, 3, 4, 5}
local s16 = table.concat(t16, ",", 2, 4)
print("Test 16 - table.concat range: " .. tostring(s16 == "2,3,4"))

-- Test 17: table.move
local t17src = {1, 2, 3, 4, 5}
local t17dst = {}
table.move(t17src, 2, 4, 1, t17dst)
print("Test 17 - table.move: " .. tostring(t17dst[1] == 2 and t17dst[2] == 3 and t17dst[3] == 4))

-- Test 18: table.move within same table
local t18 = {1, 2, 3, 4, 5}
table.move(t18, 1, 3, 4)  -- copy elements 1-3 to positions 4-6
print("Test 18 - table.move same: " .. tostring(t18[4] == 1 and t18[5] == 2 and t18[6] == 3))

print("All table library tests complete!")
