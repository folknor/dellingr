-- Regression coverage for typed Lua-pattern captures and end-position matches.
local a, b, pos, value, finish = string.find("abc", "()(a)()")
local mpos, mvalue, mend = string.match("abc", "()(a)()")
local end_start, end_finish = string.find("abc", "$", 4)
local end_match = string.match("", "$")
local replaced = string.gsub("ab", "()a", "<%0:%1:%%>")
local percent = string.gsub("50%", "%%", " percent")
local backref_pos, backref = string.match("aa", "()(a)%2")
local upper = string.match("E", "%E")
local vertical_tab = string.match("\11", "%s")
local many_positions = {string.match("x", "()()()()()()()()()()()()()()()()()()()()()()()()()()()()()()()()")}

local count = 0
for _ in string.gmatch("abc", "$") do count = count + 1 end

print("pattern result handling: " .. tostring(
    a == 1 and b == 1 and pos == 1 and value == "a" and finish == 2
    and mpos == 1 and mvalue == "a" and mend == 2
    and end_start == 4 and end_finish == 3 and end_match == ""
    and replaced == "<a:1:%>b" and count == 1
    and percent == "50 percent" and backref_pos == 1 and backref == "a" and upper == "E"
    and vertical_tab == "\11" and #many_positions == 32
))
