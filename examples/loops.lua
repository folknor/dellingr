-- Test break statement in various loop types

-- Test 1: break in while loop
print("Test 1: while loop with break")
local i = 0
while true do
    i = i + 1
    if i >= 5 then
        break
    end
end
print("i after while break:", i)

-- Test 2: break in numeric for loop
print("Test 2: for loop with break")
local found = 0
for j = 1, 100 do
    if j == 7 then
        found = j
        break
    end
end
print("found:", found)

-- Test 3: break in repeat...until loop
print("Test 3: repeat loop with break")
local k = 0
repeat
    k = k + 1
    if k == 3 then
        break
    end
until k >= 10
print("k after repeat break:", k)

-- Test 4: nested loops with break (only breaks inner)
print("Test 4: nested loops")
local outer_count = 0
local inner_total = 0
for outer = 1, 3 do
    outer_count = outer_count + 1
    for inner = 1, 10 do
        inner_total = inner_total + 1
        if inner == 2 then
            break
        end
    end
end
print("outer_count:", outer_count)
print("inner_total:", inner_total)

-- Test 5: break with no iterations after
print("Test 5: immediate break")
local x = 0
while x < 100 do
    x = x + 1
    break
end
print("x after immediate break:", x)

-- Test 6: generic for with ipairs
print("Test 6: for k, v in ipairs")
local arr = {10, 20, 30, 40, 50}
local sum = 0
local count = 0
for idx, val in ipairs(arr) do
    sum = sum + val
    count = count + 1
end
print("ipairs sum:", sum)
print("ipairs count:", count)

-- Test 7: generic for with pairs (key-value)
print("Test 7: for k, v in pairs")
local tbl = {a = 1, b = 2, c = 3}
local key_count = 0
local val_sum = 0
for k, v in pairs(tbl) do
    key_count = key_count + 1
    val_sum = val_sum + v
end
print("pairs key_count:", key_count)
print("pairs val_sum:", val_sum)

-- Test 8: break in generic for loop
print("Test 8: break in generic for")
local arr2 = {5, 10, 15, 20, 25}
local found_val = 0
for i, v in ipairs(arr2) do
    if v == 15 then
        found_val = v
        break
    end
end
print("found_val:", found_val)

-- Test 9: empty table iteration
print("Test 9: empty table")
local empty = {}
local empty_count = 0
for k, v in pairs(empty) do
    empty_count = empty_count + 1
end
print("empty_count:", empty_count)

-- Test 10: single variable generic for
print("Test 10: single var generic for")
local arr3 = {100, 200, 300}
local single_sum = 0
for val in ipairs(arr3) do
    single_sum = single_sum + val
end
print("single_sum (indices):", single_sum)

print("All loop tests complete!")
