-- brokkr verdict workload derived from examples/tables/sort.lua (see that
-- file for the workload description). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds
-- in a release build.

local nums = {}
local strs = {}
local seed = 12345
for i = 1, 2000 do
    seed = (seed * 16807) % 2147483647
    nums[i] = seed % 100000
    strs[i] = "s" .. (seed % 7919)
end

local function kernel()
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

local K = 105

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/sort: true")
