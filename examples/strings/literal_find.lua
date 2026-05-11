-- Literal string.find kernel.
-- Exercises default-pattern calls whose patterns contain no magic
-- characters, plus one-byte needles.

local log = "request_id=req-7Fa2 method=GET status=200 path=/api/v1/users size=1532b"
local needles = {
    "request_id",
    "status=200",
    "/api/v1/users",
    " ",
    "missing",
}

function _bench()
    local total = 0

    for i = 1, 1000 do
        local needle = needles[(i - 1) % #needles + 1]
        local start_pos, end_pos = string.find(log, needle)
        if start_pos then
            total = total + start_pos + end_pos
        else
            total = total + 1
        end
    end

    return total
end

for i = 1, 100 do _bench() end
print("strings/literal_find: true")
