-- Hypothesis: global assignment has no inline cache. GET_GLOBAL caches an
-- entry index validated by `globals_version`, but SET_GLOBAL does UTF-8
-- validation, a fresh String allocation, and an IndexMap hash lookup on every
-- write, then re-checks `Builtin::from_name` on the way through.
-- `tally = tally + 1` pays the cached read and the uncached write in the same
-- statement, so the remaining cost here is the write side.
-- Compare against fields/same_obj_write.lua, which measures cached field
-- writes on a table rather than globals.

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
print("globals/write: true")
