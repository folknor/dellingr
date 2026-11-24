-- Harness scenario: invoke_typed
-- Rust-to-Lua calls with arguments and a result through `State::invoke`
-- (`(f64, f64)` in, one `f64` out), the host-boundary cost a host pays per
-- entity per tick. Pairs with invoke_stack: same `step`, same count.

count = 60000000

function step(a, b)
    return a * 2 + b
end

-- The driver calls step(i, 0.5) for i = 0, count - 1 and sets `result` to
-- the sum of the results.
local n = count
local expected = n * (n - 1) + n * 0.5
function verify()
    if result ~= expected then
        error("result " .. tostring(result) .. ", expected " .. tostring(expected))
    end
end
