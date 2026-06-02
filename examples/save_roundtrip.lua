local function make_counter()
    local n = 10
    return function(delta)
        n = n + delta
        return n
    end, function()
        return n
    end
end

local inc, get = make_counter()
local cyc = {}
cyc.self = cyc
cyc.inc = inc
local m = math
local floor = math.floor

local ok = true
ok = ok and get() == 10
ok = ok and inc(5) == 15
ok = ok and get() == 15
ok = ok and cyc.self == cyc
ok = ok and cyc.inc(1) == 16
ok = ok and floor(3.9) == 3
ok = ok and m.floor(4.9) == 4

print("save roundtrip shape: " .. tostring(ok))
