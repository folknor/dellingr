-- Probe: vararg functions collect extra arguments into the shared vararg arena.
-- This kernel expands each call's values into a table, then sums them with ipairs.

local function sum_varargs(...)
    local total = 0
    for _, v in ipairs({...}) do
        total = total + v
    end
    return total
end

function _bench()
    local total = 0
    for i = 1, 500 do
        total = total + sum_varargs(1, 2, 3, 4)
    end
    return total
end

for i = 1, 600 do _bench() end
print("calls/vararg: " .. tostring(_bench()) .. ": true")
