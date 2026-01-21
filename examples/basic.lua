-- Basic VM test: loops, tables, conditionals

local sum = 0
for i = 1, 100 do
    sum = sum + i
end
print(sum)

local t = { x = 10, y = 20 }
print(t.x + t.y)

local a = 5
local b = 3
if a > b then
    print(a)
else
    print(b)
end
