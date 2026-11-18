-- brokkr verdict workload derived from examples/numerics/cull.lua (see that
-- file for the probe rationale). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds
-- in a release build.

local N = 64
local x1, y1, x2, y2 = {}, {}, {}, {}
local seed = 12345
local function rand(n)
    seed = (seed * 16807) % 2147483647
    return seed % n
end
for i = 1, N do
    local x, y = rand(1000) - 100, rand(1000) - 100
    local w, h = 4 + rand(60), 4 + rand(60)
    x1[i], y1[i], x2[i], y2[i] = x, y, x + w, y + h
end

local function kernel()
    local hidden, misses, widest, widest_at = 0, 0, 0, 0
    for i = 1, N do
        local ax1, ay1, ax2, ay2 = x1[i], y1[i], x2[i], y2[i]
        if ax2 < 0 or ax1 > 800 or ay2 < 0 or ay1 > 800 then
            hidden = hidden + 1
        end
        for j = i + 1, N do
            if ax2 < x1[j] or x2[j] < ax1 or ay2 < y1[j] or y2[j] < ay1 then
                misses = misses + 1
            end
        end
        local w = ax2 - ax1
        if w > widest then
            widest, widest_at = w, i
        end
    end
    return hidden * 10000000 + misses * 100 + widest_at
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
print("bench/cull: true")
