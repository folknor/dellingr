-- Workload: box culling and a pairwise overlap rejection test.
-- Each box is first culled against a fixed viewport, then tested against
-- every later box with the usual separating-axis rejection,
-- `a.right < b.left or b.right < a.left or ...`. Most pairs are far apart,
-- so an early `or` operand holds and the condition jumps on a true
-- ordering: register-register for the pair test, register-literal for the
-- viewport. A widest-box scan adds a register-register ordering that jumps
-- when false. Broadphase, picking and visibility code is shaped this way.

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

function _bench()
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

for i = 1, 200 do _bench() end
print("numerics/cull: " .. tostring(_bench()) .. ": true")
