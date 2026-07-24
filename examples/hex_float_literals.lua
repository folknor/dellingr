-- Hex-float literals (C30): Lua 5.2's 0x mantissa/fraction/binary-exponent
-- grammar, checked against reference Lua by diff_test.sh.

print("hex integer: " .. tostring(0xA == 10))
print("hex uppercase: " .. tostring(0XFF == 255))
print("hex fraction: " .. tostring(0x1.8 == 1.5))
print("hex bare fraction: " .. tostring(0x.8 == 0.5))
print("hex trailing dot: " .. tostring(0x1. == 1))
print("hex exponent: " .. tostring(0x1p4 == 16))
print("hex negative exponent: " .. tostring(0x1p-2 == 0.25))
print("hex signed exponent: " .. tostring(0x1.8p+0 == 1.5))
print("hex uppercase exponent: " .. tostring(0X1.8P1 == 3))
print("hex float min positive: " .. tostring(0x1p-1074 > 0))
print("hex float max: " .. tostring(0x1.fffffffffffffp1023 == 1.7976931348623157e308))

-- string.format("%q", n) emits hex floats; the parser reads them back.
print("format q round trip: " .. tostring(0x1.8p+0 == 1.5 and 0x1p+0 == 1))
