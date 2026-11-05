-- Probe: string.format over templates that are mostly literal text, with a
-- few directives and `%%` escapes between the literal runs, so the work per
-- literal byte of the format string, not argument rendering, dominates.
-- Ratios are not representative of format-heavy code that renders many
-- numbers.

local templates = {
    "Player %s scored %d points in round %d of the regional qualifying tournament.",
    "[%s] Connection to relay station established after %d attempts; latency is nominal.",
    "Shield integrity holds at %d%% and hull integrity at %d%%, reactor output is steady.",
    "The quick brown fox jumps over the lazy dog near the riverbank at dawn, again.",
}

function _bench()
    local total = 0

    for i = 1, 100 do
        total = total + #string.format(templates[1], "Ingrid", i, i % 7)
        total = total + #string.format(templates[2], "ok", i % 5)
        total = total + #string.format(templates[3], i % 100, (i * 3) % 100)
        total = total + #string.format(templates[4])
    end

    return total
end

for i = 1, 10 do _bench() end
print("strings/format_literals: " .. tostring(_bench()) .. ": true")
