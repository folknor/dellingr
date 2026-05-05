-- Numeric indexing kernel.
-- Same dense array shape as ipairs_scan, but traversed with a numeric for loop.

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
