-- Workload: an own-field read sweep over many prebuilt wide records. Every
-- record comes from one constructor with eight fields, so each is map
-- storage (more than four hash entries) with its keys at the same positions.
-- The kernel reads four static fields of every record in turn: each field
-- site sees a new receiver on every step, with no construction or call in
-- the loop. The `while` loop keeps the sweep off the typed numeric regions,
-- which start at a numeric `for`, so every read is an ordinary GET_FIELD.

local records = {}
for i = 1, 4096 do
    records[i] = {
        x = i,
        y = i * 2,
        vx = i % 5,
        vy = i % 3,
        hp = 100 - i % 50,
        armor = i % 9,
        team = i % 2,
        level = i % 12,
    }
end
local count = #records

function _bench()
    local rs, n = records, count
    local sum = 0
    local i = 1
    while i <= n do
        local r = rs[i]
        sum = sum + r.x + r.vy + r.hp + r.level
        i = i + 1
    end
    return sum
end

for i = 1, 300 do _bench() end
print("fields/wide_sweep: " .. tostring(_bench()) .. ": true")
