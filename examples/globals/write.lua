-- Historical probe: global writes once validated and allocated names on
-- every assignment. GET_GLOBAL and SET_GLOBAL now cache slots in the real
-- global table, validating the table version before an in-place access.
-- `tally = tally + 1` exercises both caches in the same statement.
-- Compare against fields/same_obj_write.lua for cached ordinary table writes.

counter = 0
tally = 0
scratch = 0

function _bench()
    for i = 1, 1000 do
        counter = i
        scratch = i + 1
        tally = tally + 1
    end
    return counter + scratch
end

for i = 1, 900 do _bench() end
print("globals/write: " .. tostring(_bench()) .. ": true")
