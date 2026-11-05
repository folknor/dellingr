-- Proper tail calls: `return f(...)` replaces the calling activation, so each
-- of these runs far past the call-depth limit at constant depth, as in Lua
-- 5.2 and 5.4.

local DEPTH = 100000

-- Test 1: self tail recursion
local function count(n, acc)
    if n == 0 then return acc end
    return count(n - 1, acc + 1)
end
print("Test 1 - self tail recursion: " .. tostring(count(DEPTH, 0) == DEPTH))

-- Test 2: a state machine whose states hand off by tail call
local ping, pong
function ping(n, hits)
    if n == 0 then return "ping", hits end
    return pong(n - 1, hits + 1)
end
function pong(n, hits)
    if n == 0 then return "pong", hits end
    return ping(n - 1, hits)
end
local last, hits = ping(DEPTH + 1, 0)
print("Test 2 - state machine: " .. tostring(last == "pong" and hits == DEPTH / 2 + 1))

-- Test 3: method tail calls
local counter = {n = 0}
function counter:run(k)
    if k == 0 then return self.n end
    self.n = self.n + 1
    return self:run(k - 1)
end
print("Test 3 - method tail calls: " .. tostring(counter:run(DEPTH) == DEPTH))

-- Test 4: a callable table tail calling itself
local callable = setmetatable({}, {__call = function(self, k, acc)
    if k == 0 then return acc end
    return self(k - 1, acc + 2)
end})
print("Test 4 - callable tail calls: " .. tostring(callable(DEPTH, 0) == 2 * DEPTH))

-- Test 5: varargs pass through a chain of tail calls
local function relay(k, ...)
    if k == 0 then return select("#", ...), ... end
    return relay(k - 1, ...)
end
local n5, a5, b5, c5 = relay(DEPTH, "a", nil, "c")
print("Test 5 - varargs through tail calls: "
    .. tostring(n5 == 3 and a5 == "a" and b5 == nil and c5 == "c"))

-- Test 6: a closure capturing a local of the replaced caller
local function apply(f, k)
    if k == 0 then return f() end
    return apply(f, k - 1)
end
local function make()
    local captured = 41
    local function read() captured = captured + 1 return captured end
    return apply(read, DEPTH)
end
print("Test 6 - captured local survives: " .. tostring(make() == 42))

-- Test 7: continuation-passing style
local function sum_cps(k, acc, done)
    if k == 0 then return done(acc) end
    return sum_cps(k - 1, acc + k, done)
end
print("Test 7 - continuation passing: "
    .. tostring(sum_cps(1000, 0, function(total) return total end) == 500500))

-- Test 8: a Rust function in tail position returns all its results
local function pieces(s) return string.find(s, "(b)(c)") end
local i8, j8, b8, c8 = pieces("abcd")
print("Test 8 - Rust tail callee results: "
    .. tostring(i8 == 2 and j8 == 3 and b8 == "b" and c8 == "c"))

-- Test 9: a parenthesised call is not a tail call and keeps one result
local function two() return 1, 2 end
local function one() return (two()) end
print("Test 9 - parenthesised call truncates: " .. tostring(select("#", one()) == 1))
