-- brokkr verdict workload derived from examples/globals/write.lua (see
-- that file for the probe rationale: SET_GLOBAL has no inline cache).
-- Same kernel, repeated K times per _bench call so one call is ~100ms and
-- the standalone run takes a few seconds in a release build.

counter = 0
tally = 0
scratch = 0

local function kernel()
    for i = 1, 1000 do
        counter = i
        scratch = i + 1
        tally = tally + 1
    end
    return counter + scratch
end

local K = 800

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/write: true")
