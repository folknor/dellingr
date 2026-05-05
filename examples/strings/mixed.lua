-- String manipulation kernel.
-- Exercises string interning, the `..` concat lowering, table.concat,
-- and a handful of string library calls. Setup builds 500 unique keys
-- once so per-iteration work focuses on concat / lookups.

local words = {
    "alpha", "beta", "gamma", "delta", "epsilon",
    "zeta", "eta", "theta", "iota", "kappa",
}

local keys = {}
for i = 1, 500 do
    keys[i] = "k_" .. i
end

function _bench()
    -- Repeated `..` concat creates many intermediate strings.
    local s = ""
    for i = 1, 200 do
        s = s .. words[(i - 1) % 10 + 1] .. "-"
    end

    -- table.concat: single allocation, no intermediates.
    local parts = {}
    for i = 1, 500 do
        parts[i] = words[(i - 1) % 10 + 1]
    end
    local joined = table.concat(parts, ",")

    -- string library calls (sub, len).
    local total = 0
    for _ = 1, 100 do
        local sub = string.sub(joined, 1, 50)
        total = total + #sub
    end

    -- Touch the prebuilt keys to keep them rooted.
    return total + #s + #joined + #keys[500]
end

for i = 1, 300 do _bench() end
print("strings/mixed: true")
