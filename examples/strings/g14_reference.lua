-- These values follow Lua 5.2's %.14g number rendering.
print("len 1/3=" .. string.len(1 / 3))
print("concat 1e15=" .. ("x" .. 1e15))

local values = {}
values[tostring(0.1 + 0.2)] = true
print("key 0.3=" .. tostring(values["0.3"]))
