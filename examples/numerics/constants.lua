-- Historical probe: kept as the regression tracker for its fix. Literal-only
-- arithmetic in a hot loop: before parse-time constant folding (commit
-- 2685dd6), `60 * 1000` and `-5` re-executed their operand loads and
-- arithmetic on every iteration, each charging cost. The parser now folds
-- each literal subexpression to one number literal, so only the
-- accumulation into `sum` runs; this file guards against folding regressing.
-- Compare against numerics/arithmetic.lua, where every operand involves a
-- loop variable and nothing is foldable - that bench is deliberately immune
-- to folding, so the pair isolates it.

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
print("numerics/constants: " .. tostring(_bench()) .. ": true")
