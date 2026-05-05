-- Hypothesis: creating a closure inside a hot loop allocates a heap
-- LuaFunction object plus upvalue slots each iteration. Should produce
-- visibly more GC pressure than equivalent inline code.

function _bench()
    local total = 0
    for i = 1, 500 do
        local x = i
        local f = function(y)
            return x + y
        end
        total = total + f(1)
    end
    return total
end
