-- Hypothesis: calling a user-defined global in a hot loop incurs a per-call
-- IndexMap lookup against `state.globals`, since only well-known builtins
-- (print, pairs, math, ...) get the Builtin-array fast path.
-- Compare against calls/local.lua, which caches the same function in a local.

function add_one(x)
    return x + 1
end

function _bench()
    local sum = 0
    for i = 1, 1000 do
        sum = add_one(sum)
    end
    return sum
end

for i = 1, 1500 do _bench() end
print("calls/global: true")
