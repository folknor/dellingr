-- Probe: the table library's metamethod checks and length. table.insert
-- and table.remove on a table with no metatable and on one whose metatable
-- defines none of the sequence events (each call asks whether the table has
-- `__len`, `__index` or `__newindex`), and table.concat and unpack, which
-- take the length through `__len` when there is one, on a plain table and
-- on a table whose `__len` handler answers.

local plain = {}
local other = setmetatable({}, { __tostring = function() return "other" end })
local words = { "a", "b", "c", "d" }
local measured = setmetatable({ "w", "x", "y", "z" }, { __len = function() return 4 end })

function _bench()
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

for i = 1, 300 do _bench() end
print("tables/sequence_len: " .. tostring(_bench()) .. ": true")
