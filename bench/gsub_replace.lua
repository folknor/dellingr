-- brokkr verdict workload derived from examples/strings/gsub_replace.lua
-- (see that file for the probe rationale). Same kernel, repeated K times
-- per _bench call so one call is ~100ms and the standalone run takes a
-- few seconds in a release build.

local parts = {}
for i = 1, 64 do
    parts[i] = "key" .. i .. "=val" .. (i * 7)
end
local line = table.concat(parts, " ")
local short = "a=1 b=2"

local function kernel()
    local total = 0

    for i = 1, 10 do
        local swapped, n1 = string.gsub(line, "(%w+)=(%w+)", "%2:%1")
        local hashed, n2 = string.gsub(line, "%d+", "#")
        local joined, n3 = string.gsub(line, " ", "_")
        local wrapped, n4 = string.gsub(short, "%a", "<%0>")
        local numbered, n5 = string.gsub(short, "%d", 0)
        total = total + n1 + n2 + n3 + n4 + n5
        total = total + #swapped + #hashed + #joined + #wrapped + #numbered
    end

    return total
end

local K = 100

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/gsub_replace: true")
