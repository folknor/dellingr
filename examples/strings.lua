-- Test string library functions

-- Test 1: string.sub basic
local s1 = "hello world"
print("Test 1 - string.sub basic: " .. tostring(string.sub(s1, 1, 5) == "hello"))

-- Test 2: string.sub negative index
print("Test 2 - string.sub negative end: " .. tostring(string.sub(s1, 7, -1) == "world"))

-- Test 3: string.sub negative start
print("Test 3 - string.sub negative start: " .. tostring(string.sub(s1, -5) == "world"))

-- Test 4: string.sub single char
print("Test 4 - string.sub single char: " .. tostring(string.sub(s1, 1, 1) == "h"))

-- Test 5: string.find plain text
local start5, end5 = string.find("hello world", "world", 1, true)
print("Test 5 - string.find plain: " .. tostring(start5 == 7 and end5 == 11))

-- Test 6: string.find from beginning
local start6, end6 = string.find("hello world", "hello")
print("Test 6 - string.find beginning: " .. tostring(start6 == 1 and end6 == 5))

-- Test 7: string.find not found
local result7 = string.find("hello world", "xyz")
print("Test 7 - string.find not found: " .. tostring(result7 == nil))

-- Test 8: string.find with pattern
local start8, end8 = string.find("hello123world", "%d+")
print("Test 8 - string.find pattern: " .. tostring(start8 == 6 and end8 == 8))

-- Test 9: string.find with capture
local start9, end9, cap9 = string.find("hello world", "(%a+) world")
print("Test 9 - string.find capture: " .. tostring(start9 == 1 and cap9 == "hello"))

-- Test 10: string.format %s
local fmt10 = string.format("hello %s", "world")
print("Test 10 - string.format %%s: " .. tostring(fmt10 == "hello world"))

-- Test 11: string.format %d
local fmt11 = string.format("value: %d", 42)
print("Test 11 - string.format %%d: " .. tostring(fmt11 == "value: 42"))

-- Test 12: string.format %f
local fmt12 = string.format("pi: %.2f", 3.14159)
print("Test 12 - string.format %%.2f: " .. tostring(fmt12 == "pi: 3.14"))

-- Test 13: string.format multiple
local fmt13 = string.format("%s is %d years old", "Alice", 30)
print("Test 13 - string.format multiple: " .. tostring(fmt13 == "Alice is 30 years old"))

-- Test 14: string.format %%
local fmt14 = string.format("100%% complete")
print("Test 14 - string.format %%%%: " .. tostring(fmt14 == "100% complete"))

-- Test 15: string.len
print("Test 15 - string.len: " .. tostring(string.len("hello") == 5))

-- Test 16: string.upper
print("Test 16 - string.upper: " .. tostring(string.upper("hello") == "HELLO"))

-- Test 17: string.lower
print("Test 17 - string.lower: " .. tostring(string.lower("HELLO") == "hello"))

-- Test 18: string.rep
print("Test 18 - string.rep: " .. tostring(string.rep("ab", 3) == "ababab"))

-- Test 19: string.reverse
print("Test 19 - string.reverse: " .. tostring(string.reverse("hello") == "olleh"))

-- Test 20: string.find with init position
local start20, end20 = string.find("hello hello", "hello", 2)
print("Test 20 - string.find with init: " .. tostring(start20 == 7 and end20 == 11))

print("All string library tests complete!")
