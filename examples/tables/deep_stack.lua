-- Probe: the length operator on a stack whose popped slots stay behind as a
-- nil tail. The stack once grew to 1024 entries, so its logical array keeps
-- that length while the live depth moves between 500 and 532; every push
-- `s[#s + 1] = v` and pop `s[#s] = nil` takes the length of a table whose
-- last array slot is nil.

local s = {}
for i = 1, 1024 do s[#s + 1] = i end
while #s > 500 do s[#s] = nil end

function _bench()
    local sum = 0
    for _ = 1, 8 do
        for i = 1, 32 do
            s[#s + 1] = i
        end
        for _ = 1, 32 do
            sum = sum + s[#s]
            s[#s] = nil
        end
    end
    return sum + #s
end

for i = 1, 300 do _bench() end
print("tables/deep_stack: " .. tostring(_bench()) .. ": " .. tostring(#s == 500))
