-- brokkr verdict workload derived from examples/strings/gmatch_bytes.lua (see
-- that file for what it measures). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds in a
-- release build.

local parts = {}
for i = 1, 64 do
    parts[i] = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ.,"
end
local subject = table.concat(parts)

local function kernel()
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

local K = 230

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/gmatch_bytes: true")
