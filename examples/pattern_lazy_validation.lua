-- Pattern validation is lazy, as in reference Lua: a malformed point is an
-- error only when a match actually reaches it, and an unclosed capture is an
-- error only when a caller reads it. Every expectation here holds in both
-- lua5.2 and lua5.4.
print("unmatched ) behind failing prefix: " .. tostring(string.match("b", "a)") == nil))
local gs, gn = string.gsub("b", "a)", "")
print("gsub keeps subject behind failing prefix: " .. tostring(gs == "b" and gn == 0))
print("unclosed ( behind failing prefix: " .. tostring(string.find("abc", "x(") == nil))
print("bad backref behind failing prefix: " .. tostring(string.find("abc", "x%1") == nil))
print("open bracket behind failing prefix: " .. tostring(string.find("abc", "x[") == nil))
print("dangling percent behind failing prefix: " .. tostring(string.find("abc", "x%") == nil))
local many = "a"
for _ = 1, 40 do many = many .. "()" end
print("too many captures behind failing prefix: " .. tostring(string.match("b", many) == nil))

-- gsub with a string replacement reads only the captures it names, so an
-- unclosed capture it never names is not an error.
local u1, n1 = string.gsub("abc", "(a", "x")
print("gsub ignores an unread unclosed capture: " .. tostring(u1 == "xbc" and n1 == 1))
local u2 = string.gsub("abc", "(a", "%0")
print("gsub whole-match reference skips unclosed capture: " .. tostring(u2 == "abc"))
local u3 = string.gsub("abc", "(a)(b", "%1")
print("gsub reads the closed capture: " .. tostring(u3 == "ac"))
local u4 = string.gsub("abc", "(a)(b", {})
print("gsub table key is the closed first capture: " .. tostring(u4 == "abc"))

-- Empty gmatch over a malformed tail never reaches it.
local seen = 0
for _ in string.gmatch("abc", "x(") do seen = seen + 1 end
print("gmatch over failing prefix yields nothing: " .. tostring(seen == 0))
