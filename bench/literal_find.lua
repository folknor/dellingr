-- brokkr verdict workload derived from examples/strings/literal_find.lua
-- (see that file for the probe rationale). Same kernel, repeated K times
-- per _bench call so one call is ~100ms and the standalone run takes a
-- few seconds in a release build.

local log = "request_id=req-7Fa2 method=GET status=200 path=/api/v1/users size=1532b"
local needles = {
    "request_id",
    "status=200",
    "/api/v1/users",
    " ",
    "missing",
}

local function kernel()
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

local K = 500

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/literal_find: true")
