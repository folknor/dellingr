-- Allocates larger record-shaped table literals in a hot loop.
-- Useful for tracking constructor pre-sizing and table promotion costs.

function _bench()
    local sum = 0
    for i = 1, 500 do
        local t = {
            a = i,
            b = i + 1,
            c = i + 2,
            d = i + 3,
            e = i + 4,
            f = i + 5,
            g = i + 6,
            h = i + 7,
        }
        sum = sum + t.a + t.b + t.c + t.d + t.e + t.f + t.g + t.h
    end
    return sum
end

for i = 1, 900 do _bench() end
print("alloc/record_tables: true")
