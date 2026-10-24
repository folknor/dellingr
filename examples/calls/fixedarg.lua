-- Probe: fixed-arity four-argument calls in a tight summation loop, paired
-- with calls/vararg.lua as separate kernels that do different work.

local function sum_four(a, b, c, d)
    return a + b + c + d
end

function _bench()
    local total = 0
    for i = 1, 500 do
        total = total + sum_four(1, 2, 3, 4)
    end
    return total
end

for i = 1, 2500 do _bench() end
print("calls/fixedarg: " .. tostring(_bench()) .. ": true")
