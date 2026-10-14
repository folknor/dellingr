-- gsub and gmatch follow Lua 5.4's empty-match rule: an empty match that ends
-- where the previous match ended is skipped. Lua 5.2 accepts it, so this file
-- matches lua5.4 only (the diff harness accepts either reference per file).
local a, an = ("abc"):gsub("%a*", "-")
print("gsub skips trailing empty match: " .. tostring(a == "-" and an == 1))
local b, bn = ("hello world"):gsub("%w*", "X")
print("gsub word star: " .. tostring(b == "X X" and bn == 2))
local c, cn = ("a b"):gsub("%s*", "_")
print("gsub space star: " .. tostring(c == "_a_b_" and cn == 3))
local d, dn = ("abc"):gsub("x*", "-")
print("gsub never-matching star: " .. tostring(d == "-a-b-c-" and dn == 4))

local out = ""
for m in ("abc"):gmatch("%a*") do out = out .. "[" .. m .. "]" end
print("gmatch skips trailing empty match: " .. tostring(out == "[abc]"))
out = ""
for m in ("a,b,,c"):gmatch("[^,]*") do out = out .. "[" .. m .. "]" end
print("gmatch csv fields: " .. tostring(out == "[a][b][][c]"))

-- gmatch's optional init argument (5.4).
out = ""
for m in ("abcab"):gmatch("a", 2) do out = out .. "[" .. m .. "]" end
print("gmatch init skips the first a: " .. tostring(out == "[a]"))
out = ""
for m in ("abc"):gmatch("", -1) do out = out .. "[" .. m .. "]" end
print("gmatch negative init: " .. tostring(out == "[][]"))
local past = 0
for _ in ("abc"):gmatch("", 10) do past = past + 1 end
print("gmatch init past the end yields nothing: " .. tostring(past == 0))
out = ""
for m in ("ab cd"):gmatch("%f[%a]%a+", 2) do out = out .. "[" .. m .. "]" end
print("gmatch init preserves frontier context: " .. tostring(out == "[cd]"))
