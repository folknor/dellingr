-- Allocates table literals with positional fields in a hot loop: a pure
-- list and a list with enough keyed fields to presize the hash part.
-- Useful for tracking how a constructor sizes its array part.

function _bench()
    local sum = 0
    for i = 1, 500 do
        local list = { i, i + 1, i + 2, i + 3, i + 4, i + 5, i + 6, i + 7, i + 8, i + 9 }
        local mixed = { i, i + 1, i + 2, i + 3, i + 4, i + 5, a = i, b = i, c = i, d = i, e = i }
        sum = sum + list[1] + list[10] + #list + mixed[6] + mixed.e + #mixed
    end
    return sum
end

for i = 1, 900 do _bench() end
print("alloc/list_literals: true")
