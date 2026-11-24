-- Probe: the GET_TABLE key-position hint on its hit path. The kernel is
-- `column_key` with every loop written as `repeat`, so no typed numeric
-- region (which starts at a numeric `for`) takes it and each `us[i][key]`
-- runs as an ordinary GET_TABLE with a string key. Records share a
-- constructor (six fields, map storage), so the key sits at one position in
-- every receiver: the site's hint misses once when the column changes and
-- hits on the other 63 reads.

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
    local round = 1
    repeat
        local c = 1
        repeat
            local key = cs[c]
            local sum = 0
            local i = 1
            repeat
                sum = sum + us[i][key]
                i = i + 1
            until i > 64
            total = total + sum * c
            c = c + 1
        until c > 6
        round = round + 1
    until round > 10
    return total
end

for i = 1, 1000 do _bench() end
print("tables/key_sweep: " .. tostring(_bench()) .. ": true")
