-- Harness scenario: invoke_stack
-- The same Rust-to-Lua call as invoke_typed through the stack sequence: push
-- two numbers, `call_anchor`, `to_number`, `pop`. Same `step`, same count.

count = 200000

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
