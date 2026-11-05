-- brokkr verdict workload derived from examples/tables/sequence_len.lua (see
-- that file for the probe rationale: the table library's sequence-metamethod
-- check and its `__len`-aware length, with and without a handler). Same
-- kernel, repeated K times per _bench call so one call is ~100ms and the
-- standalone run takes a few seconds in a release build.

local plain = {}
local other = setmetatable({}, { __tostring = function() return "other" end })
local words = { "a", "b", "c", "d" }
local measured = setmetatable({ "w", "x", "y", "z" }, { __len = function() return 4 end })

local function kernel()
    local sum = 0
    for i = 1, 200 do
        table.insert(plain, i)
        table.insert(other, i)
        sum = sum + table.remove(plain) + table.remove(other)
        local a, b = table.unpack(words)
        local c, d, e, f = table.unpack(measured)
        if f == "z" then
            sum = sum + #table.concat(words) + #table.concat(measured)
                + #a + #b + #c + #d + #e + #f
        end
    end
    return sum
end

local K = 350

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/sequence_len: true")
