-- Numeric indexing kernel.
-- Same dense array shape as iter/ipairs.lua, but traversed with a numeric for loop.

local arr = {}
for i = 1, 10000 do
    arr[i] = i
end

function _bench()
    local sum = 0
    for i = 1, 10000 do
        sum = sum + arr[i]
    end
    return sum
end

for i = 1, 250 do _bench() end
print("tables/numeric_index: true")
