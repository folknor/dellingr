-- Regression coverage for call-base marking when the callee expression is
-- already on the stack before the call frame is marked.
-- Every case needs BOTH a receiver that is not a plain name AND an argument
-- that is itself a call, which is what makes the argument count dynamic.
local function id(value) return value end

local paren_literal = ("ab"):find(id("b"))
print("paren literal receiver: " .. tostring(paren_literal == 2))

local constructor = ({ f = function(self, x) return x end }):f(id("q"))
print("constructor receiver: " .. tostring(constructor == "q"))

local paren_call = (id("ab")):find(id("b"))
print("parenthesized call receiver: " .. tostring(paren_call == 2))

local concat = ("a" .. "b"):find(id("b"))
print("concat receiver: " .. tostring(concat == 2))

-- A plain name still works: it pushes itself after the mark.
local subject = "ab"
local plain = subject:find(id("b"))
print("plain name receiver: " .. tostring(plain == 2))

-- Field and index receivers were already handled, but keep them covered.
local holder = { s = "ab", [1] = "ab" }
print("field receiver: " .. tostring(holder.s:find(id("b")) == 2))
print("index receiver: " .. tostring(holder[1]:find(id("b")) == 2))

-- Several dynamic arguments, and a nested call inside the argument.
print("two call args: " .. tostring(("abc"):find(id("b"), id(1)) == 2))
print("nested call arg: " .. tostring(("ab"):find(id(("b"):sub(id(1)))) == 2))

-- Non-method calls with an already-pushed callee.
local direct = (id(function(x) return x end))(id("v"))
print("parenthesized callee: " .. tostring(direct == "v"))

-- Varargs reaching a method call on a parenthesized receiver.
local function forward(...) return ("ab"):find(...) end
print("vararg arguments: " .. tostring(forward("b") == 2))

-- Chained shapes, where the callee is the result of another call. These are
-- the cases parentheses hide: (id("ab")):find(...) parses as a parenthesized
-- expression, so it never exercises a direct call-result callee.
local function make(x) return function(y) return x .. tostring(y) end end
local function one() return 1 end
local methods = { make = function(self) return function(y) return y end end }

print("call result as callee: " .. tostring(make("a")(id("b")) == "ab"))
print("unparenthesized call receiver: " .. tostring(id("ab"):find(id("b")) == 2))
print("method result as callee: " .. tostring(methods:make()(id("ok")) == "ok"))
-- Both calls dynamic: the inner one must not consume the outer marker.
print("dynamic inner call: " .. tostring(make(one())(id(2)) == "12"))

local callables = { [1] = function(y) return y end }
print("indexed callee: " .. tostring(callables[1]("x") == "x"))
print("indexed callee chain: " .. tostring(callables[1](id("y")) == "y"))
