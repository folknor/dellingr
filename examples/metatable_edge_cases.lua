-- Metatable Edge Case Tests
-- Tests edge cases and potential error scenarios for metamethod handling

print("=== Metatable Edge Cases ===")
print("")

-- 1. Chained __index (multiple levels of fallback)
print("1. Chained __index (3 levels deep)")
local level3 = { deep = "found at level 3" }
local level2 = setmetatable({}, { __index = level3 })
local level1 = setmetatable({}, { __index = level2 })
local obj = setmetatable({ own = "own value" }, { __index = level1 })
print("obj.own = " .. obj.own)
print("obj.deep = " .. obj.deep)
print("obj.missing = " .. tostring(obj.missing))  -- nil
print("")

-- 2. __index returning nil explicitly vs missing key
print("2. __index function returning nil")
local explicit_nil = setmetatable({}, {
    __index = function(t, k)
        if k == "exists" then
            return nil  -- explicitly return nil
        end
        -- implicitly return nil for other keys
    end
})
print("explicit_nil.exists = " .. tostring(explicit_nil.exists))  -- nil
print("explicit_nil.other = " .. tostring(explicit_nil.other))    -- nil
print("")

-- 3. __newindex with rawset to store values
print("3. __newindex with rawset (avoid infinite recursion)")
local logged = {}
local logging_table = setmetatable({}, {
    __newindex = function(t, k, v)
        logged[#logged + 1] = k .. "=" .. tostring(v)
        rawset(t, k, v)  -- must use rawset to actually store
    end,
    __index = function(t, k)
        return rawget(t, k)
    end
})
logging_table.x = 10
logging_table.y = 20
print("logging_table.x = " .. tostring(logging_table.x))
print("logging_table.y = " .. tostring(logging_table.y))
print("log: " .. table.concat(logged, ", "))
print("")

-- 4. __call with multiple return values
print("4. __call with multiple returns")
local multi_ret = setmetatable({}, {
    __call = function(self, a, b)
        return a + b, a - b, a * b
    end
})
local sum, diff, prod = multi_ret(10, 3)
print("multi_ret(10, 3) = " .. tostring(sum) .. ", " .. tostring(diff) .. ", " .. tostring(prod))
print("")

-- 5. __len on nested tables
print("5. __len with custom counting")
local nested = setmetatable({
    data = { 1, 2, 3, 4, 5 }
}, {
    __len = function(t)
        local count = 0
        for _ in pairs(t.data) do
            count = count + 1
        end
        return count
    end
})
print("#nested = " .. tostring(#nested))
print("")

-- 6. __tostring returning non-string (should work with tostring conversion)
print("6. __tostring edge cases")
local num_string = setmetatable({}, {
    __tostring = function(t)
        return "custom: [table]"
    end
})
print("tostring(num_string) = " .. tostring(num_string))
print("")

-- 7. Metatable on metatable
print("7. Metatable on metatable (meta-meta)")
local meta_meta = {
    __index = function(t, k)
        return "meta-meta: " .. k
    end
}
local meta = setmetatable({
    known = "known value"
}, meta_meta)
local obj2 = setmetatable({}, { __index = meta })
print("obj2.known = " .. obj2.known)
print("obj2.unknown = " .. obj2.unknown)  -- goes through meta's __index
print("")

-- 8. getmetatable returns the actual metatable
print("8. getmetatable returns actual table")
local mt = { __index = { x = 1 } }
local obj3 = setmetatable({}, mt)
local retrieved = getmetatable(obj3)
print("getmetatable returns same table: " .. tostring(retrieved == mt))
print("")

-- 9. Setting metatable to nil removes it
print("9. setmetatable(t, nil) removes metatable")
local obj4 = setmetatable({ val = 10 }, { __index = { val = 999 } })
print("before nil: obj4.missing = " .. tostring(obj4.missing or "nil"))
setmetatable(obj4, nil)
print("after nil: obj4.missing = " .. tostring(obj4.missing or "nil"))
print("")

-- 10. __call on nested callable
print("10. Nested __call")
local inner_callable = setmetatable({}, {
    __call = function(self, x)
        return x * 2
    end
})
local outer_callable = setmetatable({ inner = inner_callable }, {
    __call = function(self, x)
        return self.inner(x) + 1
    end
})
print("outer_callable(5) = " .. tostring(outer_callable(5)))  -- (5*2)+1 = 11
print("")

print("=== All edge case tests passed! ===")
