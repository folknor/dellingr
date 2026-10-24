-- Historical probe, kept as the regression tracker for its fix. Global
-- assignment once had no inline cache: every SET_GLOBAL paid name
-- validation, an allocation and a hash lookup while reads were already
-- cached. Per-site global write caches (commit 7ee46fd) made a warmed write
-- a validated in-place store; this file guards against the uncached path
-- coming back. `tally = tally + 1` pays a cached read and a cached write in
-- the same statement.
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
