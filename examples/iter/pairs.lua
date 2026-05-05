-- pairs iteration kernel.
-- Prebuilds a hash table once. Each benchmark call only iterates it, isolating
-- generic-for and table-next behavior.

local hash = {}
for i = 1, 2500 do
    hash["k_" .. i] = i
end

function _bench()
    local sum = 0
    for _, v in pairs(hash) do
        sum = sum + v
    end
    return sum
end

for i = 1, 600 do _bench() end
print("iter/pairs: true")
