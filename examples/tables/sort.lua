-- Default-comparison table.sort kernel.
-- Sorts fresh copies of a number list and a string list with no comparator,
-- so the time goes to the heap sort and its built-in comparison.

local nums = {}
local strs = {}
local seed = 12345
for i = 1, 2000 do
    seed = (seed * 16807) % 2147483647
    nums[i] = seed % 100000
    strs[i] = "s" .. (seed % 7919)
end

function _bench()
    local a = {}
    for i = 1, #nums do
        a[i] = nums[i]
    end
    table.sort(a)

    local s = {}
    for i = 1, #strs do
        s[i] = strs[i]
    end
    table.sort(s)

    return a[1] + #s[#s]
end

local ok = true
for i = 1, 20 do
    _bench()
end
local a = {}
for i = 1, #nums do a[i] = nums[i] end
table.sort(a)
for i = 2, #a do
    if a[i - 1] > a[i] then ok = false end
end
print("tables/sort: " .. tostring(ok))
