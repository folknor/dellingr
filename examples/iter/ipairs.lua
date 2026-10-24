-- Probe: sum a prebuilt dense array through the ipairs generic-for loop. The
-- VM has a direct table-iteration fast path for its standard iterator.

local arr = {}
for i = 1, 10000 do
    arr[i] = i
end

function _bench()
    local sum = 0
    for _, v in ipairs(arr) do
        sum = sum + v
    end
    return sum
end

for i = 1, 200 do _bench() end
print("iter/ipairs: " .. tostring(_bench()) .. ": true")
