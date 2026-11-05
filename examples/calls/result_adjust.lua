-- Probe: fixing a call's result and argument counts. Lua and Rust callees
-- whose results are padded with nil (`local a, b, c = one()`) or cut
-- (`local a = three()`), and a non-vararg Lua callee given surplus
-- arguments, which admission drops.

local function one(x)
    return x
end

local function three(x)
    return x, x + 1, x + 2
end

local function two_params(a, b)
    return a + b
end

local floor = math.floor

function _bench()
    local sum = 0
    for i = 1, 400 do
        local a, b, c = one(i)
        local d = three(i)
        local e = two_params(i, 1, 2, 3, 4)
        local f, g, h = floor(i + 0.5)
        if b == nil and c == nil and g == nil and h == nil then
            sum = sum + a + d + e + f
        end
    end
    return sum
end

for i = 1, 300 do _bench() end
print("calls/result_adjust: " .. tostring(_bench()) .. ": true")
