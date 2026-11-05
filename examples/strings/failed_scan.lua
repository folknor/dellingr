-- Probe: the pattern matcher's start-position scan. Each search tries
-- nearly every start position of a few-hundred-byte subject before it
-- matches or gives up (a class with no hit, a capture that never opens,
-- one match at the end, short words separated by spaces), so the work
-- per start position, not the work per matched byte, dominates. Ratios are
-- not representative of pattern-heavy code.

local words = {}
for i = 1, 48 do
    words[i] = "lorem"
end
local text = table.concat(words, " ")
local tagged = text .. " id=42"

function _bench()
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

for i = 1, 10 do _bench() end
print("strings/failed_scan: " .. tostring(_bench()) .. ": true")
