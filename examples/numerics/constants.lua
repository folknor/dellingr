-- Probe: literal-only arithmetic in a hot loop.
-- There is no constant folding in the parser today, so `60 * 1000` emits
-- PUSH_NUM + PUSH_NUM + MUL and `-5` emits PUSH_NUM + NEGATE on every
-- execution, each charging cost. A folding pass collapses all of these to a
-- single PUSH_NUM.
-- Compare against numerics/arithmetic.lua, where every operand involves a
-- loop variable and nothing is foldable - that bench is deliberately immune
-- to this optimization, so the pair isolates the folding win.

function _bench()
    local sum = 0
    for i = 1, 1000 do
        sum = sum + 60 * 1000
        sum = sum - 24 * 60
        sum = sum + -5
        sum = sum + 2 * 3 * 7
        sum = sum + 1024 / 4
    end
    return sum
end

for i = 1, 700 do _bench() end
print("numerics/constants: true")
