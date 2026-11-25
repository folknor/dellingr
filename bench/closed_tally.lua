-- brokkr verdict workload derived from examples/numerics/closed_tally.lua
-- (see that file for what it measures). Same kernel, repeated K times per
-- _bench call so one call is ~100ms and the standalone run takes a few
-- seconds in a release build. The captures live in a block that ends before
-- the footer, so every timed call reads and writes them closed.

do
    local sum = 0
    local wraps = 0
    local limit = 997

    local function kernel()
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
print("bench/closed_tally: true")
