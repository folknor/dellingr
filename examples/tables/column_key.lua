-- Workload: one stat read by name across every record of one layout.
-- Records share a constructor (six fields, map storage); the kernel sums a
-- column chosen at run time, `us[i][key]`, so one dynamic string-key read
-- site sees the same key across 64 receivers before the key changes.
-- Aggregating a named stat over a roster or inventory is shaped this way.

local units = {}
for i = 1, 64 do
    units[i] = {
        hp = 100 - i,
        armor = i % 9,
        speed = i % 7 + 1,
        range = i % 4 * 10,
        level = i % 12,
        team = i % 2,
    }
end
local columns = { "hp", "armor", "speed", "range", "level", "team" }

function _bench()
    local us, cs = units, columns
    local total = 0
    for round = 1, 10 do
        for c = 1, 6 do
            local key = cs[c]
            local sum = 0
            for i = 1, 64 do
                sum = sum + us[i][key]
            end
            total = total + sum * c
        end
    end
    return total
end

for i = 1, 1000 do _bench() end
print("tables/column_key: " .. tostring(_bench()) .. ": true")
