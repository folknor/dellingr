-- Negative zero renders as "-0" (Lua 5.2's spelling) through both tostring
-- and concatenation. Lua 5.4 splits by subtype, so lua5.2 is the reference.
local z = 0
local nz = z * -1
print(nz)
print(tostring(nz))
print("x" .. nz)
print(math.ceil(-0.5))
print("tostring negative zero: " .. tostring(tostring(nz) == "-0"))
print("concat negative zero: " .. tostring(("" .. nz) == "-0"))
