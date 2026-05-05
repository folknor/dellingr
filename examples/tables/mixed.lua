-- Table create/get/set kernel.
-- Exercises both array-style integer keys and string-keyed hash entries,
-- plus ipairs/pairs iteration. Allocates per-call to put pressure on GC.

local hash_keys = {}
for i = 1, 1000 do
    hash_keys[i] = "k_" .. i
end

function _bench()
    -- Array fill (sequential integer keys).
    local arr = {}
    for i = 1, 5000 do
        arr[i] = i * 2
    end

    -- Hash fill: string keys, nested table values.
    local hash = {}
    for i = 1, 1000 do
        hash[hash_keys[i]] = { id = i, value = i * 3 }
    end

    -- ipairs sum (array iteration).
    local arr_sum = 0
    for _, v in ipairs(arr) do
        arr_sum = arr_sum + v
    end

    -- pairs sum (hash iteration with field access).
    local hash_sum = 0
    for _, entry in pairs(hash) do
        hash_sum = hash_sum + entry.id
    end

    return arr_sum + hash_sum
end

for i = 1, 130 do _bench() end
print("tables/mixed: true")
