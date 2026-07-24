local function build(...)
    local earlier = {}
    local result = {earlier, ...}
    return #result == 3 and result[1] == earlier and result[2] == 8 and result[3] == 9
end

print("table multivalue preceding table: " .. tostring(build(8, 9)))
