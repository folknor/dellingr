-- brokkr verdict workload derived from examples/strings/mixed.lua (see
-- that file for the probe rationale). Same kernel, repeated K times per
-- _bench call so one call is ~100ms and the standalone run takes a few
-- seconds in a release build.

local words = {
    "alpha", "beta", "gamma", "delta", "epsilon",
    "zeta", "eta", "theta", "iota", "kappa",
}

local keys = {}
for i = 1, 500 do
    keys[i] = "k_" .. i
end

local function kernel()
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

local K = 800

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/mixed: true")
