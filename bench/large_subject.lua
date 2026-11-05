-- brokkr verdict workload derived from examples/strings/large_subject.lua
-- (see that file for the probe rationale). Same kernel, repeated K times
-- per _bench call so one call is ~100ms and the standalone run takes a
-- few seconds in a release build.

local parts = {}
for i = 1, 4096 do
    parts[i] = "field" .. i .. "=value;"
end
local big = table.concat(parts)

local function kernel()
    local total = 0

    for i = 1, 20 do
        local _, finish = string.find(big, "field1=", 1, true)
        total = total + finish
        local key = string.match(big, "^(%a+)")
        total = total + #key
        for digits in string.gmatch(big, "%d+") do
            total = total + #digits
            break
        end
        local _, count = string.gsub(big, "^%a+", "x")
        total = total + count
    end

    return total
end

local K = 400

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/large_subject: true")
