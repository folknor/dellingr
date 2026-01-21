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

-- tonumber tests
print(tonumber("42"))      -- 42
print(tonumber("3.14"))    -- 3.14
print(tonumber("hello"))   -- nil

-- tostring tests
print(tostring(123))       -- 123
print(tostring(true))      -- true
print(tostring(nil))       -- nil
