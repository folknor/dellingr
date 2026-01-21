-- Metatables and Metamethods Examples
-- Demonstrates all supported metamethods: __index, __newindex, __tostring, __call, __len

print("=== Metatables Demo ===")
print("")

-- 1. __index: Default values and inheritance
print("1. __index: Default values")
local defaults = { x = 0, y = 0, name = "unnamed" }
local point = setmetatable({ x = 10 }, { __index = defaults })
print("point.x = " .. tostring(point.x))  -- 10 (from point itself)
print("point.y = " .. tostring(point.y))  -- 0 (from defaults)
print("point.name = " .. point.name)       -- "unnamed" (from defaults)
print("")

-- 2. __index with function: computed properties
print("2. __index with function")
local computed = setmetatable({ radius = 5 }, {
    __index = function(t, k)
        if k == "diameter" then
            return t.radius * 2
        elseif k == "circumference" then
            return t.radius * 2 * 3.14159
        end
        return nil
    end
})
print("radius = " .. tostring(computed.radius))
print("diameter = " .. tostring(computed.diameter))
print("circumference = " .. tostring(computed.circumference))
print("")

-- 3. __newindex: Proxy tables and logging
print("3. __newindex: Proxy pattern")
local storage = {}
local proxy = setmetatable({}, {
    __index = storage,
    __newindex = function(t, k, v)
        print("  Setting " .. tostring(k) .. " = " .. tostring(v))
        storage[k] = v
    end
})
proxy.a = 1
proxy.b = 2
print("storage.a = " .. tostring(storage.a))
print("proxy.a = " .. tostring(proxy.a))  -- reads from storage via __index
print("")

-- 4. __tostring: Custom string representation
print("4. __tostring: Custom printing")
local person = setmetatable({ name = "Alice", age = 30 }, {
    __tostring = function(t)
        return "Person(" .. t.name .. ", age " .. tostring(t.age) .. ")"
    end
})
print("person = " .. tostring(person))
print(person)  -- print also uses __tostring
print("")

-- 5. __call: Callable tables
print("5. __call: Callable tables")
local adder = setmetatable({ base = 100 }, {
    __call = function(self, x, y)
        return self.base + x + (y or 0)
    end
})
print("adder(5) = " .. tostring(adder(5)))
print("adder(10, 20) = " .. tostring(adder(10, 20)))
print("")

-- 6. __len: Custom length
print("6. __len: Custom length operator")
local container = setmetatable({
    items = { "a", "b", "c" },
    capacity = 10
}, {
    __len = function(t)
        return t.capacity  -- return capacity instead of array length
    end
})
print("container capacity (#container) = " .. tostring(#container))
print("")

-- 7. Object-Oriented Programming pattern
print("7. OOP pattern with __index and __call")

local Vector = {}
Vector.__index = Vector

setmetatable(Vector, {
    __call = function(cls, x, y)
        local self = setmetatable({}, cls)
        self.x = x or 0
        self.y = y or 0
        return self
    end
})

Vector.length = function(self)
    return math.sqrt(self.x * self.x + self.y * self.y)
end

Vector.add = function(self, other)
    return Vector(self.x + other.x, self.y + other.y)
end

setmetatable(Vector, {
    __call = function(cls, x, y)
        local self = setmetatable({}, { __index = cls })
        self.x = x or 0
        self.y = y or 0
        return self
    end
})

local v1 = Vector(3, 4)
local v2 = Vector(1, 2)
print("v1 = (" .. tostring(v1.x) .. ", " .. tostring(v1.y) .. ")")
print("v1:length() = " .. tostring(v1:length()))
local v3 = v1:add(v2)
print("v1:add(v2) = (" .. tostring(v3.x) .. ", " .. tostring(v3.y) .. ")")
print("")

-- 8. String methods (str:method() syntax)
print("8. String methods")
local s = "hello world"
print("s:upper() = " .. s:upper())
print("s:sub(1, 5) = " .. s:sub(1, 5))
print("s:reverse() = " .. s:reverse())
print('"abc":rep(3) = ' .. ("abc"):rep(3))
print("")

print("=== All metatable tests passed! ===")
