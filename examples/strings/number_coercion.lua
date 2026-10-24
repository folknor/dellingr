-- The string library coerces number subjects and patterns to strings.
-- gsub(121, 1, "x") replaces both 1s, giving "x2x".
local found = string.find(12345, 34)
local iter = string.gmatch(121, 1)
local first = iter()
local replaced = string.gsub(121, 1, "x")
local result = string.sub(123, 2) .. ":" .. string.len(42) .. ":"
    .. string.upper(12) .. ":" .. string.lower(12) .. ":"
    .. string.reverse(123) .. ":" .. found .. ":"
    .. string.match(123, 23) .. ":" .. first .. ":" .. replaced
print(result)
