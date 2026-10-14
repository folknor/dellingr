-- A `(` on a new line continues the previous expression as a call, with or
-- without a comment between the lines (Lua 5.2 and 5.4 agree).

local function id(x)
  return x
end

local function maker()
  return function(n)
    return n + 1
  end
end

local a = id
(10)
print("plain newline: " .. tostring(a == 10))

local b = id -- trailing comment
(11)
print("line comment: " .. tostring(b == 11))

local c = id
-- comment on its own line
(12)
print("comment line: " .. tostring(c == 12))

local d = id --[[ multi
line comment ]] (13)
print("long comment: " .. tostring(d == 13))

local e = maker()
(1)
print("call result called: " .. tostring(e == 2))

local t = { f = id }
local g = t.f
(14)
print("field callee: " .. tostring(g == 14))

local h = (maker)
()
(5)
print("parenthesized callee: " .. tostring(h == 6))
