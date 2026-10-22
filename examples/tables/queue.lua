-- FIFO queue kernel.
-- A long-lived queue written the obvious way (q[tail] = v; q[head] = nil)
-- that keeps a small backlog while its keys climb forever, with an occasional
-- pairs walk over it. The queue persists across calls, so its history grows
-- with every call: storage or traversal work that tracks history rather than
-- the live backlog shows up as calls getting slower over a run.

local q, head, tail = {}, 1, 0

function _bench()
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

for i = 1, 30 do _bench() end
print("tables/queue: " .. tostring(q[head] ~= nil and q[tail] ~= nil and q[head - 1] == nil))
