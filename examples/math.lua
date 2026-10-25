-- Test math library
-- Whole-valued floats print as Lua 5.2 does ("0", not 5.4's "0.0"). The
-- random stream is dellingr's own, so only its ranges are checked here.

print("=== Testing math library ===")

-- Test math.pi
print("math.pi =", math.pi)

-- Test trigonometric functions
print("\n=== Trigonometric functions ===")
print("math.sin(0) =", math.sin(0))
print("math.sin(math.pi/2) =", math.sin(math.pi / 2))
print("math.cos(0) =", math.cos(0))
print("math.cos(math.pi) =", math.cos(math.pi))
print("math.atan2(1, 1) =", math.atan2(1, 1))
print("math.atan2(0, 1) =", math.atan2(0, 1))

-- Test math.sqrt
print("\n=== Square root ===")
print("math.sqrt(4) =", math.sqrt(4))
print("math.sqrt(16) =", math.sqrt(16))
print("math.sqrt(2) =", math.sqrt(2))

-- Test math.abs
print("\n=== Absolute value ===")
print("math.abs(5) =", math.abs(5))
print("math.abs(-5) =", math.abs(-5))
print("math.abs(0) =", math.abs(0))

-- Test math.min and math.max
print("\n=== Min and Max ===")
print("math.min(3, 7) =", math.min(3, 7))
print("math.min(10, 2) =", math.min(10, 2))
print("math.max(3, 7) =", math.max(3, 7))
print("math.max(10, 2) =", math.max(10, 2))

-- Test math.floor and math.ceil
print("\n=== Floor and Ceil ===")
print("math.floor(3.7) =", math.floor(3.7))
print("math.floor(3.2) =", math.floor(3.2))
print("math.floor(-3.7) =", math.floor(-3.7))
print("math.ceil(3.2) =", math.ceil(3.2))
print("math.ceil(3.7) =", math.ceil(3.7))
print("math.ceil(-3.2) =", math.ceil(-3.2))

-- Test math.random
print("\n=== Random ===")
local unit_ok, upper_ok, range_ok = true, true, true
for _ = 1, 200 do
    local u = math.random()
    if not (u >= 0 and u < 1) then unit_ok = false end
    local n = math.random(10)
    if not (n >= 1 and n <= 10 and n == math.floor(n)) then upper_ok = false end
    local r = math.random(5, 10)
    if not (r >= 5 and r <= 10 and r == math.floor(r)) then range_ok = false end
end
print("math.random() is in [0, 1): " .. tostring(unit_ok))
print("math.random(10) is an integer in [1, 10]: " .. tostring(upper_ok))
print("math.random(5, 10) is an integer in [5, 10]: " .. tostring(range_ok))

print("\n=== All tests completed ===")
