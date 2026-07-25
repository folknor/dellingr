-- Control expressions must resolve names in the enclosing scope.

local i = 5
do
    local a, b, c, i = 0, 0, 0, 2
end
local numeric = {}
for i = i, 7 do
    numeric[#numeric + 1] = i
end
print("numeric control scope: " .. tostring(numeric[1] == 5 and numeric[2] == 6 and numeric[3] == 7))

local t = {"outer"}
do
    local a, b, c, d, t = 0, 0, 0, 0, {"stale"}
end
local generic = {}
for _, t in ipairs(t) do
    generic[#generic + 1] = t
end
print("generic control scope: " .. tostring(generic[1] == "outer" and generic[2] == nil))

-- table.move is absent in Lua 5.2, so this forces the whole file to match Lua 5.4.
local moved = {}
table.move({42}, 1, 1, 1, moved)
print("Lua 5.4 signature: " .. tostring(moved[1] == 42))
