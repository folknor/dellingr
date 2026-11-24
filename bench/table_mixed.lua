-- brokkr verdict workload derived from examples/tables/mixed.lua (see
-- that file for what it measures). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds in a
-- release build.

local hash_keys = {}
for i = 1, 1000 do
    hash_keys[i] = "k_" .. i
end

local function kernel()
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

local K = 330

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/table_mixed: true")
