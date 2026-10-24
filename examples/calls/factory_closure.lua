-- Historical probe: factory closures, mk() returning a new closure on
-- every call. Inline caches once lived per closure, so every produced
-- closure paid a cold-cache miss on its first field access. They now live
-- per State and bytecode, shared by every closure of the same function, so
-- a fresh closure starts warm; this file guards against per-closure caches
-- coming back.

local function mk()
    return function(t)
        return t.x + t.y
    end
end

function _bench()
    local t = { x = 1, y = 2 }
    local sum = 0
    for i = 1, 1000 do
        local f = mk()
        sum = sum + f(t)
    end
    return sum
end

for i = 1, 1500 do _bench() end
print("calls/factory_closure: " .. tostring(_bench()) .. ": true")
