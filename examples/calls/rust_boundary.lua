-- Workload: calls made from Rust into Lua - function handlers, library
-- callbacks, iterators and callable resolution - in the same proportions
-- in both workload files.
local indexed = setmetatable({}, {__index = function(_, k) return k end})
local callable = setmetatable({}, {__call = setmetatable({}, {
    __call = function(_, _, x) return x end
})})
local function less(a, b) return a < b end
local function replacement() return 'b' end
local function iterator(_, n) if n < 3 then return n + 1 end end
function _bench()
    local sum = 0
    for i = 1, 100 do
        sum = sum + indexed[i]
        local t = {3, 1, 2}
        table.sort(t, less)
        local s = string.gsub('aaa', 'a', replacement)
        for n in iterator, nil, 0 do sum = sum + n end
        sum = sum + callable(t[1]) + #s
    end
    return sum
end
for i = 1, 30 do _bench() end
print("calls/rust_boundary: " .. tostring(_bench()) .. ": true")
