-- Probe: `math.cos` alone, over the angles game scripts pass it (headings
-- within a few turns of zero). Isolates the cosine kernel in src/crmath, so
-- its ratios are not representative of a mixed math workload.

function _bench()
    local acc = 0
    for i = 1, 2000 do
        acc = acc + math.cos(i * 0.00314159) + math.cos(i * -0.0271828)
    end
    return acc
end

for i = 1, 300 do _bench() end
print("numerics/cos: " .. tostring(_bench()) .. ": true")
