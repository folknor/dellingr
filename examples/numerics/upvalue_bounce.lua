-- Workload: an integer bounce simulation reading its tuning constants as
-- upvalues. Every step reads several captured numbers (gravity, drag, the
-- floor and the ceiling), and a typed loop region does not take a loop that
-- uses a captured number as a number, so each read dispatches a GET_UPVAL.
-- The captures are declared in a block that ends before the footer, so they
-- are closed whenever `_bench` runs, as tuning constants in a loaded module
-- are.

do
    local gravity = 3
    local drag = 1
    local floor = 0
    local ceiling = 900

    function _bench()
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
end

for i = 1, 1000 do _bench() end
print("numerics/upvalue_bounce: " .. tostring(_bench()) .. ": true")
