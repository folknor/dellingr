-- Hypothesis: vararg functions go through dynamic ArgCount handling and
-- the vararg_call_bases stack. A tight loop calling a vararg function
-- should expose any per-call overhead vs a fixed-arity equivalent.

local function sum_varargs(...)
    local total = 0
    for _, v in ipairs({...}) do
        total = total + v
    end
    return total
end

function _bench()
    local total = 0
    for i = 1, 500 do
        total = total + sum_varargs(1, 2, 3, 4)
    end
    return total
end
