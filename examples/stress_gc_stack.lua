-- Stress test: GC with tables on stack
-- This tests the scenario where GC might free a table that's still on the stack
-- Pattern: create table, keep reference, trigger GC, verify table still valid

print("Starting GC stack stress test...")

-- Test 1: Many nested function calls with tables
local function deep_tables(depth)
    local t = { depth = depth, data = {} }
    for i = 1, 10 do
        t.data[i] = { x = i, y = i * 2 }
    end
    if depth > 0 then
        -- Create more garbage to trigger GC
        for i = 1, 100 do
            local garbage = { a = i, b = {} }
        end
        local result = deep_tables(depth - 1)
        -- Verify our table is still valid after recursive call
        if t.depth ~= depth then
            error("Table corrupted at depth " .. depth)
        end
        return result + t.depth
    end
    return t.depth
end

local result = deep_tables(50)
print("Deep tables result:", result)

-- Test 2: Metatables with GC pressure
local function test_metatables()
    local mt = {
        __index = function(t, k)
            -- Allocate garbage during metamethod
            for i = 1, 50 do
                local g = { x = i }
            end
            return k .. "_value"
        end
    }

    for i = 1, 500 do
        local t = setmetatable({}, mt)
        local v = t.test  -- Triggers __index metamethod
        if v ~= "test_value" then
            error("Metatable broken at iteration " .. i)
        end
    end
end

test_metatables()
print("Metatable test done")

-- Test 3: ipairs with GC pressure
local function test_ipairs_gc()
    local arr = {}
    for i = 1, 100 do
        arr[i] = { id = i, name = "item" .. i }
    end

    local count = 0
    for i, v in ipairs(arr) do
        -- Create garbage during iteration
        for j = 1, 20 do
            local g = { x = j }
        end
        if v.id ~= i then
            error("Array corrupted at index " .. i)
        end
        count = count + 1
    end

    if count ~= 100 then
        error("Expected 100 iterations, got " .. count)
    end
end

test_ipairs_gc()
print("ipairs GC test done")

-- Test 4: Multiple tables on stack simultaneously
local function multi_table_stack()
    for i = 1, 200 do
        local t1 = { a = 1 }
        local t2 = { b = 2 }
        local t3 = { c = 3 }
        local t4 = { d = 4 }

        -- Create GC pressure
        for j = 1, 50 do
            local g = { garbage = j }
        end

        -- Verify all tables still valid
        if t1.a ~= 1 or t2.b ~= 2 or t3.c ~= 3 or t4.d ~= 4 then
            error("Multi-table corruption at iteration " .. i)
        end
    end
end

multi_table_stack()
print("Multi-table stack test done")

print("PASS - All GC stack stress tests completed")
