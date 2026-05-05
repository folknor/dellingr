-- Hypothesis: allocating short-lived tables in a hot loop pressures the
-- mark-sweep GC. Useful baseline for "is GC the bottleneck?" questions.
-- Each iteration allocates a fresh 4-entry table and discards it.

function _bench()
    local sum = 0
    for i = 1, 500 do
        local t = { a = i, b = i + 1, c = i + 2, d = i + 3 }
        sum = sum + t.a + t.b + t.c + t.d
    end
    return sum
end
