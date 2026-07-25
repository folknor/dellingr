-- Hypothesis: every Lua call re-interns ALL of the callee chunk's string
-- literals into state.string_literals, then truncates them on return. The cost
-- is proportional to the literal count of the function, not to how many
-- literals the call actually touches.
-- `classify` below carries ~32 distinct string constants but returns from the
-- first branch on every call here, so essentially all of that interning work is
-- pure per-call overhead. Caching the interned literals per (State, Bytecode)
-- makes this bench collapse toward calls/local.lua.
-- Compare against calls/local.lua: same local-call shape, one literal.

local function classify(code)
    if code == 0 then return "ok" end
    if code == 1 then return "err_perm" end
    if code == 2 then return "err_noent" end
    if code == 3 then return "err_srch" end
    if code == 4 then return "err_intr" end
    if code == 5 then return "err_io" end
    if code == 6 then return "err_nxio" end
    if code == 7 then return "err_2big" end
    if code == 8 then return "err_noexec" end
    if code == 9 then return "err_badf" end
    if code == 10 then return "err_child" end
    if code == 11 then return "err_again" end
    if code == 12 then return "err_nomem" end
    if code == 13 then return "err_acces" end
    if code == 14 then return "err_fault" end
    if code == 15 then return "err_notblk" end
    if code == 16 then return "err_busy" end
    if code == 17 then return "err_exist" end
    if code == 18 then return "err_xdev" end
    if code == 19 then return "err_nodev" end
    if code == 20 then return "err_notdir" end
    if code == 21 then return "err_isdir" end
    if code == 22 then return "err_inval" end
    if code == 23 then return "err_nfile" end
    if code == 24 then return "err_mfile" end
    if code == 25 then return "err_notty" end
    if code == 26 then return "err_txtbsy" end
    if code == 27 then return "err_fbig" end
    if code == 28 then return "err_nospc" end
    if code == 29 then return "err_spipe" end
    if code == 30 then return "err_rofs" end
    return "unknown"
end

function _bench()
    local hits = 0
    for i = 1, 1000 do
        if classify(0) == "ok" then
            hits = hits + 1
        end
    end
    return hits
end

for i = 1, 700 do _bench() end
print("calls/many_literals: true")
