-- Upvalue Stress Tests
-- Tests edge cases and stress scenarios for upvalue/closure handling

print("=== Upvalue Stress Tests ===")
print("")

-- 1. Many upvalues in one closure
print("1. Many upvalues (10 captured variables)")
local a, b, c, d, e, f, g, h, i, j = 1, 2, 3, 4, 5, 6, 7, 8, 9, 10
local sum_all = function()
    return a + b + c + d + e + f + g + h + i + j
end
print("sum of 1-10 = " .. tostring(sum_all()))  -- 55
a = 100
print("after a=100, sum = " .. tostring(sum_all()))  -- 154
print("")

-- 2. Deeply nested closures sharing upvalues
print("2. Deeply nested closures (5 levels)")
local outer = 1
local make_nested = function()
    local level1 = 10
    return function()
        local level2 = 100
        return function()
            local level3 = 1000
            return function()
                local level4 = 10000
                return function()
                    return outer + level1 + level2 + level3 + level4
                end
            end
        end
    end
end
local deep = make_nested()()()()
print("deeply nested sum = " .. tostring(deep()))  -- 11111
outer = 2
print("after outer=2, sum = " .. tostring(deep()))  -- 11112
print("")

-- 3. Many closures sharing same upvalue
print("3. Many closures sharing one upvalue")
local shared = 0
local closures = {}
local make_incrementer = function(idx)
    return function()
        shared = shared + idx
        return shared
    end
end
for i = 1, 5 do
    closures[i] = make_incrementer(i)
end
print("shared starts at: " .. tostring(shared))
for i = 1, 5 do
    print("closure " .. tostring(i) .. "() = " .. tostring(closures[i]()))
end
print("shared ends at: " .. tostring(shared))  -- 0+1+2+3+4+5 = 15
print("")

-- 4. Closure escaping its creating function
print("4. Closure escaping creator (counter pattern)")
local make_counter = function(start)
    local count = start
    return {
        inc = function() count = count + 1 return count end,
        dec = function() count = count - 1 return count end,
        get = function() return count end,
        set = function(v) count = v end
    }
end
local c1 = make_counter(0)
local c2 = make_counter(100)
print("c1: " .. tostring(c1.inc()) .. ", " .. tostring(c1.inc()) .. ", " .. tostring(c1.inc()))  -- 1, 2, 3
print("c2: " .. tostring(c2.dec()) .. ", " .. tostring(c2.dec()))  -- 99, 98
print("c1.get() = " .. tostring(c1.get()))  -- 3
print("c2.get() = " .. tostring(c2.get()))  -- 98
c1.set(50)
print("after c1.set(50): c1.get() = " .. tostring(c1.get()))  -- 50
print("")

-- 5. Upvalue from loop variable
print("5. Upvalues from loop variables")
local funcs = {}
for i = 1, 3 do
    local capture = i  -- capture loop variable in a local
    funcs[i] = function() return capture end
end
print("funcs[1]() = " .. tostring(funcs[1]()))  -- 1
print("funcs[2]() = " .. tostring(funcs[2]()))  -- 2
print("funcs[3]() = " .. tostring(funcs[3]()))  -- 3
print("")

-- 6. Mutual recursion through upvalues
print("6. Mutual recursion via upvalues")
local is_even, is_odd
is_even = function(n)
    if n == 0 then return true end
    return is_odd(n - 1)
end
is_odd = function(n)
    if n == 0 then return false end
    return is_even(n - 1)
end
print("is_even(10) = " .. tostring(is_even(10)))  -- true
print("is_odd(10) = " .. tostring(is_odd(10)))    -- false
print("is_even(7) = " .. tostring(is_even(7)))    -- false
print("is_odd(7) = " .. tostring(is_odd(7)))      -- true
print("")

-- 7. Upvalue shadowing
print("7. Upvalue shadowing")
local x = "outer"
local get_outer = function() return x end
local test_shadow = function()
    local x = "inner"  -- shadows outer x
    local get_inner = function() return x end
    return get_outer() .. ", " .. get_inner()
end
print("shadowing result: " .. test_shadow())  -- "outer, inner"
print("")

-- 8. Closure as table value being called
print("8. Closures in tables")
local base = 1000
local ops = {
    add = function(x) return base + x end,
    sub = function(x) return base - x end,
    mul = function(x) return base * x end
}
print("ops.add(5) = " .. tostring(ops.add(5)))    -- 1005
print("ops.sub(5) = " .. tostring(ops.sub(5)))    -- 995
print("ops.mul(5) = " .. tostring(ops.mul(5)))    -- 5000
base = 2000
print("after base=2000:")
print("ops.add(5) = " .. tostring(ops.add(5)))    -- 2005
print("")

-- 9. Self-modifying closure
print("9. Self-modifying closure")
local make_accumulator = function()
    local total = 0
    local count = 0
    return function(x)
        total = total + x
        count = count + 1
        return total, count
    end
end
local acc = make_accumulator()
local t, c
t, c = acc(10)
print("acc(10): total=" .. tostring(t) .. ", count=" .. tostring(c))
t, c = acc(20)
print("acc(20): total=" .. tostring(t) .. ", count=" .. tostring(c))
t, c = acc(30)
print("acc(30): total=" .. tostring(t) .. ", count=" .. tostring(c))
print("")

print("=== All upvalue stress tests passed! ===")
