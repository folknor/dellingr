-- Test math library
-- DIFF: number_format - Rust formats floats differently (precision, trailing zeros)

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
print("math.random() =", math.random())
print("math.random() =", math.random())
print("math.random(10) =", math.random(10))
print("math.random(10) =", math.random(10))
print("math.random(5, 10) =", math.random(5, 10))
print("math.random(5, 10) =", math.random(5, 10))

print("\n=== All tests completed ===")
