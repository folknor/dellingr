-- brokkr verdict workload derived from examples/tables/queue.lua (see that
-- file for the probe rationale). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds
-- in a release build.

local q, head, tail = {}, 1, 0

local function kernel()
    local acc = 0
    for i = 1, 2000 do
        tail = tail + 1
        q[tail] = i
        if tail - head >= 16 then
            acc = acc + q[head]
            q[head] = nil
            head = head + 1
        end
        if i % 100 == 0 then
            for _, v in pairs(q) do
                acc = acc + v
            end
        end
    end
    return acc
end

local K = 200

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/queue: true")
