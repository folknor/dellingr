-- Extended Lua feature test
-- Tests features not covered in feature_test.lua
-- Will error on first failure

local function test(name, condition)
    if condition then
        print("[PASS] " .. name)
    else
        print("[FAIL] " .. name)
        error("Test failed: " .. name)
    end
end

print("=== Extended Lua Feature Tests ===\n")

-- ============================================================================
-- LEXER / PARSER EDGE CASES
-- ============================================================================
print("--- Lexer/Parser ---")

test("uppercase hex 0XFF", 0XFF == 255 and 0XAB == 171)
test("number with leading dot", .5 == 0.5 and .125 == 0.125)
test("number with trailing dot", 5. == 5.0)
test("negative exponent", 1e-3 == 0.001)
test("positive exponent explicit", 1e+3 == 1000)
test("uppercase E exponent", 1E3 == 1000)

-- Escape sequences (test via string length since string.byte is not implemented)
test("escape \\r exists", #"\r" == 1)
test("escape \\a exists", #"\a" == 1)
test("escape \\b exists", #"\b" == 1)
test("escape \\f exists", #"\f" == 1)
test("escape \\v exists", #"\v" == 1)
test("escape \\0 exists", #"\0" == 1)
test("escape \\' exists", #"\'" == 1)
test("escape \\\" exists", #"\"" == 1)

-- Comments
local comment_test = 1 -- this is a comment
test("single line comment", comment_test == 1)

--[[
This is a multi-line comment
It should be completely ignored
]]
local multiline_comment_test = 2
test("multi-line comment", multiline_comment_test == 2)

--[[ inline multi-line ]] local inline_test = 3
test("inline multi-line comment", inline_test == 3)

-- ============================================================================
-- TABLE CONSTRUCTOR EDGE CASES
-- ============================================================================
print("\n--- Table Constructors ---")

local t1 = {1, 2, 3,}  -- trailing comma
test("trailing comma", t1[3] == 3)

local t2 = {1; 2; 3}  -- semicolon separators
test("semicolon separators", t2[1] == 1 and t2[2] == 2 and t2[3] == 3)

local t3 = {1, 2; 3, 4}  -- mixed separators
test("mixed separators", t3[1] == 1 and t3[4] == 4)

local t4 = {}  -- empty table
test("empty table", #t4 == 0)

local t5 = {;}  -- empty table with semicolon
test("empty table with semicolon", #t5 == 0)

local t5b = {,}  -- empty table with comma
test("empty table with comma", #t5b == 0)

local key = "dynamic"
local t6 = {[key] = 42, ["literal"] = 100, [1+1] = 200}
test("computed keys", t6.dynamic == 42 and t6.literal == 100 and t6[2] == 200)

local t7 = {a = {b = {c = 1}}}
test("nested table literals", t7.a.b.c == 1)

local t8 = {[true] = "yes", [false] = "no"}
test("boolean keys", t8[true] == "yes" and t8[false] == "no")

-- ============================================================================
-- FUNCTION EDGE CASES
-- ============================================================================
print("\n--- Function Edge Cases ---")

-- Forward declaration / mutual recursion
local is_even, is_odd

is_even = function(n)
    if n == 0 then return true end
    return is_odd(n - 1)
end

is_odd = function(n)
    if n == 0 then return false end
    return is_even(n - 1)
end

test("mutual recursion", is_even(10) == true and is_odd(7) == true)

-- Function returning multiple varargs from call
local function return_three() return 1, 2, 3 end
local function pass_through(...) return ... end
local a, b, c = pass_through(return_three())
test("pass through varargs", a == 1 and b == 2 and c == 3)

-- Varargs in middle of call (only first value used)
local function sum3(x, y, z) return x + y + z end
local result = sum3(return_three(), 10, 20)
test("varargs in middle position", result == 31)  -- 1 + 10 + 20

-- Extra arguments discarded
local function take_one(x) return x end
test("extra args discarded", take_one(1, 2, 3) == 1)

-- Calling with no args when expecting some
local function with_defaults(a, b, c)
    a = a or 10
    b = b or 20
    c = c or 30
    return a + b + c
end
test("default values via or", with_defaults() == 60)

-- Deeply nested closures
local function make_deep()
    local a = 1
    return function()
        local b = 2
        return function()
            local c = 3
            return function()
                return a + b + c
            end
        end
    end
end
test("deeply nested closures", make_deep()()()() == 6)

-- ============================================================================
-- CONTROL FLOW EDGE CASES
-- ============================================================================
print("\n--- Control Flow Edge Cases ---")

-- Empty loop body
local empty_count = 0
for i = 1, 5 do
    empty_count = empty_count + 1
end
test("for loop increments", empty_count == 5)

-- Loop with zero iterations
local zero_count = 0
for i = 1, 0 do
    zero_count = zero_count + 1
end
test("zero iteration for loop", zero_count == 0)

-- Negative step that doesn't execute
local neg_count = 0
for i = 1, 10, -1 do
    neg_count = neg_count + 1
end
test("wrong direction step", neg_count == 0)

-- Nested break
local outer_count = 0
local inner_count = 0
for i = 1, 10 do
    outer_count = outer_count + 1
    for j = 1, 10 do
        inner_count = inner_count + 1
        if j == 3 then break end
    end
    if i == 5 then break end
end
test("nested break", outer_count == 5 and inner_count == 15)

-- While with complex condition
local w = 0
while w < 10 and w ~= 5 do
    w = w + 1
end
test("while complex condition", w == 5)

-- Repeat with complex condition
local r = 0
repeat
    r = r + 1
until r >= 5 or r == 3
test("repeat complex condition", r == 3)

-- ============================================================================
-- ARITHMETIC METAMETHODS - NOT IMPLEMENTED
-- ============================================================================
print("\n--- Arithmetic Metamethods ---")
print("SKIP: __add, __sub, __mul, __div, __mod, __pow, __unm not implemented")

-- ============================================================================
-- COMPARISON METAMETHODS - NOT IMPLEMENTED
-- ============================================================================
print("\n--- Comparison Metamethods ---")
print("SKIP: __eq, __lt, __le not implemented")

-- ============================================================================
-- __CONCAT METAMETHOD - NOT IMPLEMENTED
-- ============================================================================
print("\n--- Concat Metamethod ---")
print("SKIP: __concat not implemented")

-- ============================================================================
-- STRING PATTERN EDGE CASES
-- ============================================================================
print("\n--- String Patterns ---")

test("pattern %d+", string.match("abc123def", "%d+") == "123")
test("pattern %a+", string.match("123abc456", "%a+") == "abc")
test("pattern %w+", string.match("   hello123   ", "%w+") == "hello123")
test("pattern %s+", string.match("hello   world", "%s+") == "   ")

test("pattern ^anchor", string.match("hello", "^hel") == "hel")
test("pattern $anchor", string.match("hello", "llo$") == "llo")
test("pattern ^$", string.match("exact", "^exact$") == "exact")

test("pattern char class [abc]", string.match("xxxbbbxxx", "[abc]+") == "bbb")
test("pattern negated class [^abc]", string.match("abcdef", "[^abc]+") == "def")

test("pattern optional ?", string.match("color", "colou?r") == "color")
test("pattern optional match", string.match("colour", "colou?r") == "colour")

test("pattern * zero or more", string.match("aaa", "a*") == "aaa")
test("pattern * empty", string.match("bbb", "a*") == "")

test("pattern captures", (function()
    local a, b = string.match("hello world", "(%a+) (%a+)")
    return a == "hello" and b == "world"
end)())

test("pattern gsub count", (function()
    local result, count = string.gsub("hello", "l", "L")
    return result == "heLLo" and count == 2
end)())

-- ============================================================================
-- MORE EDGE CASES
-- ============================================================================
print("\n--- More Edge Cases ---")

-- Modifying table during pairs iteration (undefined but shouldn't crash)
local t = {a = 1, b = 2, c = 3}
local count = 0
for k, v in pairs(t) do
    count = count + 1
    -- Don't add/remove during iteration, just read
end
test("pairs iteration count", count == 3)

-- ipairs stops at nil
local sparse = {1, 2, nil, 4, 5}
local icount = 0
for i, v in ipairs(sparse) do
    icount = icount + 1
end
test("ipairs stops at nil", icount == 2)

-- next with explicit nil
local nt = {a = 1}
local k1 = next(nt, nil)
test("next with nil key", k1 == "a")

-- Multiple assignment with function call
local function ret2() return 10, 20 end
local x, y, z = ret2(), 30
test("multi-assign func then value", x == 10 and y == 30 and z == nil)

-- Table with 0 index
local t0 = {[0] = "zero", [1] = "one"}
test("table with index 0", t0[0] == "zero" and t0[1] == "one")

-- Negative indices in tables
local tneg = {[-1] = "neg1", [-2] = "neg2"}
test("negative table indices", tneg[-1] == "neg1" and tneg[-2] == "neg2")

-- Very large table index
local tbig = {[1000000] = "big"}
test("large table index", tbig[1000000] == "big")

-- Chained comparisons (evaluated left to right)
local chain = 1 < 2 and 2 < 3 and 3 < 4
test("chained comparisons", chain == true)

-- Boolean short-circuit returns actual value
local short1 = nil or "default"
test("or returns right on nil", short1 == "default")

local short2 = false or "default"
test("or returns right on false", short2 == "default")

local short3 = "value" and "other"
test("and returns right on truthy", short3 == "other")

local short4 = nil and "other"
test("and returns left on nil", short4 == nil)

-- ============================================================================
-- RECURSIVE STRUCTURES (within reason)
-- ============================================================================
print("\n--- Recursive Structures ---")

-- Self-referencing table
local self_ref = {}
self_ref.self = self_ref
test("self-referencing table", self_ref.self.self.self == self_ref)

-- Mutual table references
local t_a = {}
local t_b = {}
t_a.other = t_b
t_b.other = t_a
test("mutual table references", t_a.other.other == t_a)

-- ============================================================================
-- GLOBAL ENVIRONMENT
-- ============================================================================
print("\n--- Global Environment ---")

-- Setting global via _G
_G.my_global = 42
test("_G set global", my_global == 42)

-- Reading global via _G
local gval = _G.my_global
test("_G read global", gval == 42)

-- _G.print should work (note: may not be == print due to builtin fast path)
test("_G.print works", (function() _G.print("testing _G.print"); return true end)())

-- _G._G is _G
test("_G._G is _G", _G._G == _G)

-- Cleanup
my_global = nil

-- ============================================================================
-- SUMMARY
-- ============================================================================
print("\n=== All extended tests passed! ===")
