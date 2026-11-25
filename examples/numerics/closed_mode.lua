-- Workload: a tally kept in closed captures, updated in a loop that also
-- tests a captured string mode. The string comparison is no number branch,
-- so a typed loop region declines the loop and every `GET_UPVAL` and
-- `SET_UPVAL` dispatches per instruction, as in a callback whose state
-- outlived its defining frame. `closed_tally` is the same tally without
-- the mode test, which a region takes.

do
    local sum = 0
    local wraps = 0
    local limit = 997
    local mode = "wrap"

    function _bench()
        sum = 0
        wraps = 0
        for i = 1, 2000 do
            sum = sum + i
            if mode == "wrap" and sum > limit then
                sum = sum - limit
                wraps = wraps + 1
            end
        end
        return wraps * 1000 + sum
    end
end

for i = 1, 1000 do _bench() end
print("numerics/closed_mode: " .. tostring(_bench()) .. ": true")
