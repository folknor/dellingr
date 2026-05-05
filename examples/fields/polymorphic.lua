-- Polymorphic field-read kernel.
-- Iterates a fresh table at each step, so per-callsite IC never hits.
-- Measures the steady-state polymorphic miss path.

local items = {}
for i = 1, 5000 do
    items[i] = { id = i, value = i * 2 }
end

function _bench()
    local sum = 0
    for i = 1, 5000 do
        local entry = items[i]
        sum = sum + entry.id + entry.value
    end
    return sum
end

for i = 1, 300 do _bench() end
print("fields/polymorphic: true")
