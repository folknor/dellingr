-- Probe: per-call argument handling of the pattern functions on a large
-- subject. Every call finishes after a few bytes of matching (an anchored
-- pattern, a match at the start, one gmatch step), so the work that scales
-- with the subject is what the call does with its arguments before and
-- after matching. Ratios are not representative of pattern-heavy code.

local parts = {}
for i = 1, 4096 do
    parts[i] = "field" .. i .. "=value;"
end
local big = table.concat(parts)

function _bench()
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

for i = 1, 10 do _bench() end
print("strings/large_subject: " .. tostring(_bench()) .. ": true")
