-- Probe: string and number replacements in string.gsub. Each call makes
-- many short matches over a subject of a few hundred bytes (captures
-- swapped through a template, digit runs, a plain separator) or a few
-- matches over a short subject, so the work around each match (capture
-- handoff, template application, output appends) and the per-call setup
-- weigh more than in typical code. Ratios are not representative.

local parts = {}
for i = 1, 64 do
    parts[i] = "key" .. i .. "=val" .. (i * 7)
end
local line = table.concat(parts, " ")
local short = "a=1 b=2"

function _bench()
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

for i = 1, 10 do _bench() end
print("strings/gsub_replace: " .. tostring(_bench()) .. ": true")
