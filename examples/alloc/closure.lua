-- Workload: creates and calls a closure capturing a local each loop iteration.

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

for i = 1, 1000 do _bench() end
print("alloc/closure: " .. tostring(_bench()) .. ": true")
