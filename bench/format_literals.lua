-- brokkr verdict workload derived from examples/strings/format_literals.lua
-- (see that file for the probe rationale). Same kernel, repeated K times
-- per _bench call so one call is ~100ms and the standalone run takes a
-- few seconds in a release build.

local templates = {
    "Player %s scored %d points in round %d of the regional qualifying tournament.",
    "[%s] Connection to relay station established after %d attempts; latency is nominal.",
    "Shield integrity holds at %d%% and hull integrity at %d%%, reactor output is steady.",
    "The quick brown fox jumps over the lazy dog near the riverbank at dawn, again.",
}

local function kernel()
    local total = 0

    for i = 1, 100 do
        total = total + #string.format(templates[1], "Ingrid", i, i % 7)
        total = total + #string.format(templates[2], "ok", i % 5)
        total = total + #string.format(templates[3], i % 100, (i * 3) % 100)
        total = total + #string.format(templates[4])
    end

    return total
end

local K = 150

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/format_literals: true")
