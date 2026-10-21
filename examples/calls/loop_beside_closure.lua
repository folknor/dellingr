-- Diagnostic probe: a function that builds one closure and also runs plain
-- loops whose locals nothing captures. Before per-scope capture tracking the
-- closure kept a CloseUpvalues in every iteration of those loops; compare
-- against numerics/arithmetic, whose hot functions make no closure.

function _bench()
    local scale = 3
    local scaled = function(v)
        return v * scale
    end
    local total = 0
    for i = 1, 2000 do
        local t = i * 2
        total = total + t
    end
    local j = 0
    while j < 1000 do
        local step = 1
        j = j + step
    end
    return scaled(total + j)
end

for i = 1, 300 do _bench() end
print("calls/loop_beside_closure: true")
