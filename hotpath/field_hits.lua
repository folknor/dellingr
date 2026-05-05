-- Table field read kernel.
-- Prebuilds nested records once, then repeatedly reads array slots and fields.

local items = {}
for i = 1, 5000 do
    items[i] = { id = i, value = i * 2 }
end

function _bench()
    local sum = 0
    for i = 1, 5000 do
        local entry = items[i]
        sum = sum + entry.id + entry.value
    end
    return sum
end
