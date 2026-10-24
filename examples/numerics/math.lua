-- Workload: math library kernel. Calls every transcendental the library
-- computes in crate (src/crmath, ports of CORE-MATH's correctly rounded
-- functions) plus `^`, in the shapes game scripts use them - angles,
-- headings, decay curves, falloff. The composite workload calls `math.sin`
-- in a loop too, but this is the only one that covers the whole set.

function _bench()
    local acc = 0
    for i = 1, 200 do
        local a = i * 0.0314159
        local s, c = math.sin(a), math.cos(a)
        acc = acc + s * c + math.tan(a * 0.25)
        acc = acc + math.atan2(s, c) + math.atan(a) + math.asin(s * 0.5) + math.acos(c * 0.5)
        acc = acc + math.exp(-a * 0.1) + math.log(i) + math.log(i, 10)
        acc = acc + (i * 0.5) ^ 1.5
    end
    return acc
end

for i = 1, 300 do _bench() end
print("numerics/math: " .. tostring(_bench()) .. ": true")
