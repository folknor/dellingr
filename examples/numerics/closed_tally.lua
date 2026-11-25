-- Workload: a tally kept in closed captures and updated in a loop. Every
-- step reads the running sum and the wrap limit as upvalues and writes the
-- sum, and every wrap writes the wrap count. A typed loop region takes the
-- loop, holding each written capture in its mirror and writing it back at
-- the region's exit. The captures are declared in a block that ends before
-- the footer, so they are closed whenever `_bench` runs, as the state of a
-- callback that outlived its defining frame is. `closed_mode` is the same
-- tally in a loop the region declines.

do
    local sum = 0
    local wraps = 0
    local limit = 997

    function _bench()
        sum = 0
        wraps = 0
        for i = 1, 2000 do
            sum = sum + i
            if sum > limit then
                sum = sum - limit
                wraps = wraps + 1
            end
        end
        return wraps * 1000 + sum
    end
end

for i = 1, 1000 do _bench() end
print("numerics/closed_tally: " .. tostring(_bench()) .. ": true")
