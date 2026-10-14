-- The `#` operator calls __len with two operands, both the operand itself,
-- as Lua 5.2 and 5.4 do (and as the table library's length helper does).

local seen_count, seen_a, seen_b
local t = setmetatable({}, {
    __len = function(...)
        seen_count = select("#", ...)
        seen_a, seen_b = ...
        return 7
    end,
})

print("len result: " .. tostring(#t == 7))
print("len operand count is two: " .. tostring(seen_count == 2))
print("len first operand is table: " .. tostring(seen_a == t))
print("len second operand is table: " .. tostring(seen_b == t))
