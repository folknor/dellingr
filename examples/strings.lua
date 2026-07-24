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

-- Test 18: string.reverse
print("Test 18 - string.reverse: " .. tostring(string.reverse("hello") == "olleh"))

-- Test 19: string.find with init position
local start19, end19 = string.find("hello hello", "hello", 2)
print("Test 19 - string.find with init: " .. tostring(start19 == 7 and end19 == 11))

-- Test 20: Escape sequences
print("Test 20 - escape newline: " .. tostring(string.find("hello\nworld", "\n") == 6))
print("Test 21 - escape tab: " .. tostring(string.find("hello\tworld", "\t") == 6))
print("Test 22 - escape backslash: " .. tostring(string.find("hello\\world", "\\") == 6))
print("Test 23 - escape quote: " .. tostring(string.find("hello\"world", "\"") == 6))
print("Test 24 - escape single quote: " .. tostring(string.find('hello\'world', "'") == 6))

-- Verify actual content
local with_newline = "a\nb"
print("Test 25 - newline length: " .. tostring(#with_newline == 3))
local with_tab = "a\tb"
print("Test 26 - tab length: " .. tostring(#with_tab == 3))

-- Test 27: Lua 5.2 decimal and hexadecimal string escapes
print("Test 27 - decimal escape: " .. tostring(#"\065" == 1 and "\065" == "A"))
print("Test 28 - hexadecimal escape: " .. tostring(#"\x41" == 1 and "\x41" == "A"))
print("Test 29 - byte escapes: " .. tostring("\255" == "\xFF"))
local z_joined = "left\z
    right"
print("Test 30 - z whitespace escape: " .. tostring(z_joined == "leftright"))

print("All string library tests complete!")
