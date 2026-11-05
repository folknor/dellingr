-- brokkr verdict workload derived from examples/strings/failed_scan.lua
-- (see that file for the probe rationale). Same kernel, repeated K times
-- per _bench call so one call is ~100ms and the standalone run takes a
-- few seconds in a release build.

local words = {}
for i = 1, 48 do
    words[i] = "lorem"
end
local text = table.concat(words, " ")
local tagged = text .. " id=42"

local function kernel()
    local total = 0

    for i = 1, 20 do
        if string.find(text, "%d") == nil then
            total = total + 1
        end
        if string.match(text, "(%u%l+)") == nil then
            total = total + 1
        end
        local _, count = string.gsub(tagged, "%d+", "#")
        total = total + count
        for word in string.gmatch(text, "%a+") do
            total = total + #word
        end
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
print("bench/failed_scan: true")
