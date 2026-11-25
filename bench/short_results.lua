-- brokkr verdict workload derived from examples/strings/short_results.lua
-- (see that file for the probe rationale). Same kernel, repeated K times
-- per _bench call so one call is ~100ms and the standalone run takes a
-- few seconds in a release build.

local pair = { 0, 0 }

local function kernel()
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

local K = 150

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/short_results: true")
