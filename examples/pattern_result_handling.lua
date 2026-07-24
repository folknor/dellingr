-- Regression coverage for typed Lua-pattern captures and end-position matches.
local a, b, pos, value, finish = string.find("abc", "()(a)()")
local mpos, mvalue, mend = string.match("abc", "()(a)()")
local end_start, end_finish = string.find("abc", "$", 4)
local end_match = string.match("", "$")
local replaced = string.gsub("ab", "()a", "<%0:%1:%%>")

local count = 0
for _ in string.gmatch("abc", "$") do count = count + 1 end

print("pattern result handling: " .. tostring(
    a == 1 and b == 1 and pos == 1 and value == "a" and finish == 2
    and mpos == 1 and mvalue == "a" and mend == 2
    and end_start == 4 and end_finish == 3 and end_match == ""
    and replaced == "<a:1:%>b" and count == 1
))
