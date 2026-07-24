local function values()
    return 6, 7
end

local function nested(...)
    local from_vararg = {{...}}
    local from_call = {{values()}}
    local mixed = {{...}, ...}
    return #from_vararg[1] == 2 and from_vararg[1][1] == 2 and from_vararg[1][2] == 3
        and #from_call[1] == 2 and from_call[1][1] == 6 and from_call[1][2] == 7
        and #mixed == 3 and #mixed[1] == 2 and mixed[2] == 2 and mixed[3] == 3
end

print("table multivalue nested: " .. tostring(nested(2, 3)))
