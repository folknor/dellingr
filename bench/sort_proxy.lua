-- brokkr verdict workload derived from examples/tables/sort_proxy.lua (see
-- that file for the workload description). Same kernel, repeated K times per
-- _bench call so one call is ~100ms and the standalone run takes a few
-- seconds in a release build.

local nums = {}
local seed = 12345
for i = 1, 2000 do
    seed = (seed * 16807) % 2147483647
    nums[i] = seed % 100000
end

local function kernel()
    local back = {}
    for i = 1, #nums do
        back[i] = nums[i]
    end
    local proxy = setmetatable({}, {
        __len = function() return #back end,
        __index = function(_, k) return back[k] end,
        __newindex = function(_, k, v) back[k] = v end,
    })
    table.sort(proxy)
    return back[1]
end

local K = 170

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/sort_proxy: true")
