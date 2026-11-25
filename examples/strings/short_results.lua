-- Probe: the growing producers (`string.format`, `gsub`, `table.concat`)
-- returning results of at most 22 bytes, which are interned inline, plus
-- float directives. Isolates the producers' per-call buffer and the
-- per-directive float body; ratios are not representative.

local pair = { 0, 0 }

function _bench()
    local total = 0

    for i = 1, 100 do
        total = total + #string.format("%d:%d", i, i % 7)
        total = total + #string.format("%.2f", i / 7)
        total = total + #string.format("%10.3e|%g", i * 1.5, i / 3)
        total = total + #string.gsub("a=1 b=2", "%d", i % 10)
        pair[1] = i
        pair[2] = i % 3
        total = total + #table.concat(pair, "-")
    end

    return total
end

for i = 1, 10 do _bench() end
print("strings/short_results: " .. tostring(_bench()) .. ": true")
