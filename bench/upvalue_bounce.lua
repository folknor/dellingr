-- brokkr verdict workload derived from examples/numerics/upvalue_bounce.lua
-- (see that file for what it measures). Same kernel, repeated K times per
-- _bench call so one call is ~100ms and the standalone run takes a few
-- seconds in a release build. The captures live in a block that ends before
-- the footer, so every timed call reads them closed.

do
    local gravity = 3
    local drag = 1
    local floor = 0
    local ceiling = 900

    local function kernel()
        local y, vy, bounces = 400, 0, 0
        for i = 1, 2000 do
            vy = vy - gravity
            y = y + vy
            if y < floor then
                y = floor + (floor - y)
                vy = drag - vy
                bounces = bounces + 1
            end
            if y > ceiling then
                y = ceiling - (y - ceiling)
                vy = drag - vy
            end
        end
        return y * 100000 + bounces * 100 + vy
    end

    local K = 2500

    function _bench()
        local acc = 0
        for _ = 1, K do
            acc = acc + kernel()
        end
        return acc
    end
end

for i = 1, 30 do _bench() end
print("bench/upvalue_bounce: true")
