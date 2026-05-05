-- Table write/allocation kernel.
-- Allocates fresh tables per call without generic iteration.

local keys = {}
for i = 1, 3000 do
    keys[i] = "k_" .. i
end

function _bench()
    local arr = {}
    for i = 1, 8000 do
        arr[i] = i
    end

    local hash = {}
    for i = 1, 3000 do
        hash[keys[i]] = i * 3
    end

    return arr[8000] + hash[keys[3000]]
end
