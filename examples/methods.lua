-- Test method call syntax (obj:method(args))

-- Test 1: Basic method call
local obj1 = {
    x = 10,
    getX = function(self) return self.x end
}
local r1 = obj1:getX()
print("Test 1 - Basic method call: " .. tostring(r1 == 10))

-- Test 2: Method call with arguments
local obj2 = {
    value = 5,
    add = function(self, n) return self.value + n end
}
local r2 = obj2:add(3)
print("Test 2 - Method with args: " .. tostring(r2 == 8))

-- Test 3: Method call with multiple arguments
local obj3 = {
    base = 100,
    compute = function(self, a, b) return self.base + a * b end
}
local r3 = obj3:compute(3, 4)
print("Test 3 - Method with multiple args: " .. tostring(r3 == 112))

-- Test 4: Chained method calls
local counter = {
    value = 0,
    inc = function(self)
        self.value = self.value + 1
        return self
    end,
    get = function(self) return self.value end
}
local r4 = counter:inc():inc():inc():get()
print("Test 4 - Chained method calls: " .. tostring(r4 == 3))

-- Test 5: Method returning multiple values
local obj5 = {
    x = 1,
    y = 2,
    getXY = function(self) return self.x, self.y end
}
local x, y = obj5:getXY()
print("Test 5 - Method returning multiple values: " .. tostring(x == 1 and y == 2))

-- Test 6: Method call on nested object
local outer = {
    inner = {
        val = 42,
        getVal = function(self) return self.val end
    }
}
local r6 = outer.inner:getVal()
print("Test 6 - Nested object method: " .. tostring(r6 == 42))

-- Test 7: Equivalent dot notation (should produce same result)
local obj7 = {
    x = 20,
    getX = function(self) return self.x end
}
local r7a = obj7:getX()
local r7b = obj7.getX(obj7)
print("Test 7 - Colon vs dot equivalence: " .. tostring(r7a == r7b and r7a == 20))

print("All method call tests complete!")
