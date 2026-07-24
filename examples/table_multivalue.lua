local function values()
    return 11, 22, 33
end

local function collect(...)
    local only_first = {..., 99}
    local trailing = {99, ...}
    return #only_first == 2 and only_first[1] == 4 and only_first[2] == 99
        and #trailing == 3 and trailing[1] == 99 and trailing[2] == 4 and trailing[3] == 5
end

local final_call = {7, values()}
local non_final_call = {values(), 7}
local keyed_last = {values(), k = 7}
local call_last = {k = 7, values()}

print("table multivalue: " .. tostring(
    #final_call == 4 and final_call[4] == 33
        and #non_final_call == 2 and non_final_call[1] == 11 and non_final_call[2] == 7
        and #keyed_last == 1 and keyed_last[1] == 11 and keyed_last.k == 7
        and #call_last == 3 and call_last[3] == 33
        and collect(4, 5)
))
