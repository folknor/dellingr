-- Numeric behavior that intentionally follows Lua 5.4.

local nan = 0 / 0
print("NaN ordered comparisons: " .. tostring(
    not (nan < 1) and not (nan <= 1) and not (nan > 1) and not (nan >= 1)
    and not (1 < nan) and not (1 <= nan) and not (1 > nan) and not (1 >= nan)
))
print("string ordered comparisons: " .. tostring("a" <= "a" and "b" >= "a"))

local inf = 1 / 0
print("infinite divisor modulo: " .. tostring(
    1 % inf == 1 and -1 % inf == inf and 1 % -inf == -inf and -1 % -inf == -1
))
print("opposite sign modulo: " .. tostring(5 % -3 == -1 and -5 % 3 == 1))

local positive_integral, positive_fractional = math.modf(inf)
local negative_integral, negative_fractional = math.modf(-inf)
print("modf infinite parts: " .. tostring(
    positive_integral == inf and negative_integral == -inf
    and 1 / positive_fractional == inf and 1 / negative_fractional == inf
))

-- table.move is absent in Lua 5.2, so this forces the whole file to match Lua 5.4.
local moved = {}
table.move({42}, 1, 1, 1, moved)
print("Lua 5.4 signature: " .. tostring(moved[1] == 42))
