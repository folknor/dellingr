-- ipairs iteration kernel.
-- Prebuilds one dense array, then sums it through the Rust-backed ipairs iterator.

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
