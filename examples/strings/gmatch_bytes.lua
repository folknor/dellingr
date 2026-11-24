-- Probe: a gmatch loop over many one-byte matches. One iterator per
-- traversal steps a 4 KB subject one byte at a time, so the run is mostly
-- successful iterator steps (with one terminating miss per traversal), not
-- scanning or capture work.

local parts = {}
for i = 1, 64 do
    parts[i] = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ.,"
end
local subject = table.concat(parts)

function _bench()
    local s = subject
    local n = 0
    for _ in string.gmatch(s, ".") do
        n = n + 1
    end
    if n ~= 4096 then
        error("expected 4096 matches, got " .. n)
    end
    return n
end

for i = 1, 200 do _bench() end
print("strings/gmatch_bytes: " .. tostring(_bench()) .. ": true")
