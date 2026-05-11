-- Comprehensive Lua feature test
-- Tests standard Lua features to find what's missing in our VM
-- Will error on first failure - run repeatedly to find all issues
-- DIFF: unpack_global - we provide global unpack (like lua5.2), lua5.4 only has table.unpack

local function test(name, condition)
    if condition then
        print("[PASS] " .. name)
    else
        print("[FAIL] " .. name)
        error("Test failed: " .. name)
    end
end

print("=== Lua Feature Tests ===\n")

-- ============================================================================
-- BASIC TYPES
-- ============================================================================
print("--- Basic Types ---")

test("nil type", type(nil) == "nil")
test("boolean type", type(true) == "boolean" and type(false) == "boolean")
test("number type", type(42) == "number" and type(3.14) == "number")
test("string type", type("hello") == "string")
test("table type", type({}) == "table")
test("function type", type(print) == "function")

-- ============================================================================
-- NUMBERS
-- ============================================================================
print("\n--- Numbers ---")

test("hex literals", 0xFF == 255 and 0x10 == 16)
test("scientific notation", 1e3 == 1000 and 1.5e2 == 150)
test("negative zero", -0 == 0)
test("infinity", math.huge > 0 and math.huge + 1 == math.huge)

local nan = 0/0
test("NaN not equal to itself", nan ~= nan)

-- ============================================================================
-- STRINGS
-- ============================================================================
print("\n--- Strings ---")

test("string length", #"hello" == 5 and #"" == 0)
test("string concat", "a" .. "b" == "ab")
test("escape \\n", #"\n" == 1)
test("escape \\t", #"\t" == 1)
test("escape \\\\", #"\\" == 1)

-- SKIP: long strings [[...]] not implemented yet
-- local long_str = [[hello
-- world]]
-- test("long string [[...]]", #long_str > 5)

-- SKIP: long strings [=[...]=] not implemented yet
-- local long_str2 = [=[contains [[brackets]]]=]
-- test("long string [=[...]=]", #long_str2 > 0)

test("string.len", string.len("hello") == 5)
test("string.sub positive", string.sub("hello", 1, 3) == "hel")
test("string.sub negative", string.sub("hello", -2) == "lo")
test("string.upper", string.upper("hello") == "HELLO")
test("string.lower", string.lower("HELLO") == "hello")
test("string.reverse", string.reverse("abc") == "cba")

local i, j = string.find("hello", "ll")
test("string.find", i == 3 and j == 4)

test("string.match", string.match("hello", "l+") == "ll")
test("string.gsub", string.gsub("hello", "l", "L") == "heLLo")
local empty_gsub_result, empty_gsub_count = string.gsub("abc", "", "-")
test("string.gsub empty pattern", empty_gsub_result == "-a-b-c-" and empty_gsub_count == 4)

local gmatch_count = 0
for w in string.gmatch("hello world", "%w+") do
    gmatch_count = gmatch_count + 1
end
test("string.gmatch", gmatch_count == 2)

test("string.format %d", string.format("%d", 42) == "42")
test("string.format %s", string.format("%s", "hi") == "hi")

-- ============================================================================
-- TABLES
-- ============================================================================
print("\n--- Tables ---")

local t1 = {1, 2, 3}
test("table literal array", t1[1] == 1 and t1[2] == 2 and t1[3] == 3)

local tableEntryValue = 42
local t1b = {tableEntryValue, tableEntryValue + 1}
test("table identifier array entries", t1b[1] == 42 and t1b[2] == 43)

local t2 = {a = 1, b = 2}
test("table literal keys", t2.a == 1 and t2["b"] == 2)

local t3 = {1, 2, a = 3}
test("table mixed", t3[1] == 1 and t3[2] == 2 and t3.a == 3)

test("table length", #{1, 2, 3} == 3)

local t4 = {1, 2}
table.insert(t4, 3)
test("table.insert", t4[3] == 3)

local t5 = {1, 2, 3}
local removed = table.remove(t5)
test("table.remove", removed == 3 and #t5 == 2)

test("table.concat", table.concat({"a", "b", "c"}, ",") == "a,b,c")

local t6 = {3, 1, 2}
table.sort(t6)
test("table.sort", t6[1] == 1 and t6[2] == 2 and t6[3] == 3)

local t7 = {1, 2, 3}
table.sort(t7, function(a, b) return a > b end)
test("table.sort with comparator", t7[1] == 3)

local a, b, c = table.unpack({1, 2, 3})
test("table.unpack", a == 1 and b == 2 and c == 3)

local ua, ub = unpack({1, 2, 3}, 2, 3)
test("global unpack range", ua == 2 and ub == 3)

local t8 = table.pack(1, 2, 3)
test("table.pack", t8.n == 3 and t8[1] == 1)

local t9 = {1, 2, 3}
local t10 = {}
table.move(t9, 1, 3, 1, t10)
test("table.move", t10[1] == 1 and t10[2] == 2 and t10[3] == 3)

-- ============================================================================
-- FUNCTIONS
-- ============================================================================
print("\n--- Functions ---")

local function add(x, y) return x + y end
test("local function", add(1, 2) == 3)

local dottedDecl = { inner = { value = 5 } }
function dottedDecl.inner:add(n) return self.value + n end
test("dotted method declaration", dottedDecl.inner:add(7) == 12)

local anon = function(x, y) return x + y end
test("anonymous function", anon(1, 2) == 3)

local function multi() return 1, 2, 3 end
local ma, mb, mc = multi()
test("multiple return", ma == 1 and mb == 2 and mc == 3)

local function varsum(...)
    local s = 0
    for i, v in ipairs({...}) do
        s = s + v
    end
    return s
end
test("varargs", varsum(1, 2, 3) == 6)

local function seltest(...)
    return select(2, ...)
end
local sa, sb = seltest(1, 2, 3)
test("select", sa == 2 and sb == 3)

local function selcount(...) return select("#", ...) end
test("select # count", selcount(1, 2, 3) == 3)

local function counter()
    local n = 0
    return function()
        n = n + 1
        return n
    end
end
local ctr = counter()
test("closure", ctr() == 1 and ctr() == 2 and ctr() == 3)

local function fib(n)
    if n < 2 then return n end
    return fib(n-1) + fib(n-2)
end
test("recursive local function", fib(10) == 55)

-- ============================================================================
-- CONTROL FLOW
-- ============================================================================
print("\n--- Control Flow ---")

local x1 = 0
if true then x1 = 1 end
test("if-then", x1 == 1)

local x2
if false then x2 = 1 else x2 = 2 end
test("if-else", x2 == 2)

local x3 = 2
local y3
if x3 == 1 then y3 = "one"
elseif x3 == 2 then y3 = "two"
else y3 = "other"
end
test("if-elseif-else", y3 == "two")

local wi = 0
while wi < 5 do wi = wi + 1 end
test("while", wi == 5)

local ri = 0
repeat ri = ri + 1 until ri >= 5
test("repeat-until", ri == 5)

local fsum = 0
for fi = 1, 5 do fsum = fsum + fi end
test("numeric for", fsum == 15)

local fsum2 = 0
for fi = 1, 5, 2 do fsum2 = fsum2 + fi end
test("numeric for with step", fsum2 == 9)

local fsum3 = 0
for fi = 5, 1, -1 do fsum3 = fsum3 + fi end
test("numeric for negative step", fsum3 == 15)

local psum = 0
for k, v in pairs({a = 1, b = 2}) do psum = psum + v end
test("generic for pairs", psum == 3)

local isum = 0
for i, v in ipairs({10, 20, 30}) do isum = isum + v end
test("generic for ipairs", isum == 60)

local bi = 0
while true do
    bi = bi + 1
    if bi == 5 then break end
end
test("break", bi == 5)

local bx = 1
do
    local bx = 2
end
test("do-end scope", bx == 1)

-- ============================================================================
-- OPERATORS
-- ============================================================================
print("\n--- Operators ---")

test("addition", 1 + 2 == 3)
test("subtraction", 5 - 3 == 2)
test("multiplication", 3 * 4 == 12)
test("division", 10 / 4 == 2.5)
test("modulo", 10 % 3 == 1)
test("power", 2 ^ 3 == 8)
test("negation", -5 == 0 - 5)

test("less than", 1 < 2)
test("greater than", 2 > 1)
test("less or equal", 1 <= 1)
test("greater or equal", 1 >= 1)
test("equal", 1 == 1)
test("not equal", 1 ~= 2)

test("and true", (true and true) == true)
test("and false", (true and false) == false)
test("and short-circuit", (1 and 2) == 2)
test("and nil", (nil and 2) == nil)

test("or true", (true or false) == true)
test("or false", (false or false) == false)
test("or short-circuit", (1 or 2) == 1)
test("or nil", (nil or 2) == 2)

test("not false", not false == true)
test("not true", not true == false)
test("not nil", not nil == true)

test("string less than", "a" < "b")
test("string equal", "abc" == "abc")

test("modulo negative", (-1) % 3 == 2)

-- ============================================================================
-- METATABLES
-- ============================================================================
print("\n--- Metatables ---")

local mt1 = {}
local t11 = {}
setmetatable(t11, mt1)
test("setmetatable/getmetatable", getmetatable(t11) == mt1)

local t12 = {}
setmetatable(t12, {__index = {x = 42}})
test("__index table", t12.x == 42)

local t13 = {}
setmetatable(t13, {__index = function(t, k) return k .. "!" end})
test("__index function", t13.hello == "hello!")

local nilog = {}
local t14 = {}
setmetatable(t14, {__newindex = function(t, k, v) nilog[k] = v end})
t14.x = 42
test("__newindex", nilog.x == 42 and rawget(t14, "x") == nil)

local t15 = {}
setmetatable(t15, {__tostring = function() return "custom" end})
test("__tostring", tostring(t15) == "custom")

local t16 = {}
setmetatable(t16, {__call = function(t, x) return x * 2 end})
test("__call", t16(21) == 42)

local t17 = {1, 2, 3}
setmetatable(t17, {__len = function() return 100 end})
test("__len", #t17 == 100)

local t18 = {}
setmetatable(t18, {__index = {x = 42}})
test("rawget", rawget(t18, "x") == nil)

local t19 = {}
setmetatable(t19, {__newindex = function() end})
rawset(t19, "x", 42)
test("rawset", t19.x == 42)

local t20a, t20b = {}, {}
test("rawequal", rawequal(t20a, t20a) == true and rawequal(t20a, t20b) == false)

local t21 = {1, 2, 3}
setmetatable(t21, {__len = function() return 100 end})
test("rawlen", rawlen(t21) == 3)

-- ============================================================================
-- MATH
-- ============================================================================
print("\n--- Math ---")

test("math.abs", math.abs(-5) == 5)
test("math.floor", math.floor(3.7) == 3)
test("math.ceil", math.ceil(3.2) == 4)
test("math.min", math.min(1, 2, 3) == 1)
test("math.max", math.max(1, 2, 3) == 3)
test("math.sqrt", math.sqrt(4) == 2)
test("math.sin", math.abs(math.sin(0)) < 0.001)
test("math.cos", math.abs(math.cos(0) - 1) < 0.001)
test("math.pi", math.abs(math.pi - 3.14159) < 0.001)

local r = math.random()
test("math.random()", r >= 0 and r < 1)

local r2 = math.random(1, 10)
test("math.random(a,b)", r2 >= 1 and r2 <= 10)

-- ============================================================================
-- GLOBAL
-- ============================================================================
print("\n--- Global ---")

_G.testvar123 = 42
test("_G access", testvar123 == 42)
testvar123 = nil

test("tonumber string", tonumber("42") == 42)
test("tonumber float", tonumber("3.14") == 3.14)
test("tonumber invalid", tonumber("abc") == nil)
test("tonumber base", tonumber("ff", 16) == 255 and tonumber("-z", 36) == -35)

test("tostring number", tostring(42) == "42")
test("tostring bool", tostring(true) == "true")
test("tostring nil", tostring(nil) == "nil")

local nt = {a = 1}
local nk, nv = next(nt)
test("next", nk == "a" and nv == 1)

local ua, ub, uc = unpack({1, 2, 3})
test("unpack", ua == 1 and ub == 2 and uc == 3)

-- ============================================================================
-- EDGE CASES
-- ============================================================================
print("\n--- Edge Cases ---")

local ecount = 0
for k, v in pairs({}) do ecount = ecount + 1 end
test("empty table iteration", ecount == 0)

local kt = {}
local ktt = {[kt] = "value"}
test("table as key", ktt[kt] == "value")

local kf = function() end
local kft = {[kf] = "value"}
test("function as key", kft[kf] == "value")

local chain = {
    value = 0,
    add = function(self, n) self.value = self.value + n; return self end
}
chain:add(1):add(2):add(3)
test("chained method calls", chain.value == 6)

test("string method syntax", ("hello"):upper() == "HELLO")

local function unparen_echo(value) return value end
test("unparenthesized string call", unparen_echo "hi" == "hi")
test("unparenthesized table call", unparen_echo { value = 42 }.value == 42)

local ma2, mb2 = 1, 2
ma2, mb2 = mb2, ma2
test("swap via multiple assignment", ma2 == 2 and mb2 == 1)

local function emulti() return 1, 2, 3 end
local ea = emulti()
test("extra return values discarded", ea == 1)

local function eargs(a, b, c) return a, b, c end
local ea2, eb2, ec2 = eargs(1)
test("missing args are nil", ea2 == 1 and eb2 == nil and ec2 == nil)

-- ============================================================================
-- SUMMARY
-- ============================================================================
print("\n=== All tests passed! ===")
