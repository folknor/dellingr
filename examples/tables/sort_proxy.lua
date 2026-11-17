-- Workload: table.sort through __len, __index and __newindex.
-- Sorts a proxy whose elements live in a backing table, with the default
-- comparison, so the time goes to the handler reads and writes and the heap
-- sort over the read values.

local nums = {}
local seed = 12345
for i = 1, 2000 do
    seed = (seed * 16807) % 2147483647
    nums[i] = seed % 100000
end

function _bench()
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

for i = 1, 5 do
    _bench()
end
local least = nums[1]
for i = 2, #nums do
    if nums[i] < least then least = nums[i] end
end
local ok = _bench() == least
print("tables/sort_proxy: " .. tostring(_bench()) .. ": " .. tostring(ok))
