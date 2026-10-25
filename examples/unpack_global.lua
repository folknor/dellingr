-- Global unpack exists alongside table.unpack, as in Lua 5.2 (Lua 5.4 has
-- only table.unpack). See docs/compatibility.md.

local a, b, c = unpack({1, 2, 3})
print("global unpack returns every element: " .. tostring(a == 1 and b == 2 and c == 3))

local ua, ub = unpack({1, 2, 3}, 2, 3)
print("global unpack honours a range: " .. tostring(ua == 2 and ub == 3))

print("global unpack past the end yields nil: " .. tostring(unpack({1, 2}, 3, 3) == nil))
