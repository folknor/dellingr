-- Harness scenario: many_chunks
-- One State holding many loaded chunks and a small live heap. The driver
-- compiles `chunk_source` `chunks` times as distinct Programs and runs each
-- (each roots a closure and fills its field caches once), settles, then runs
-- `count` explicit full collections with automatic collection off: the
-- per-collection work that scales with retained runtimes (the per-runtime
-- literal roots and cache sweep, plus the runtime-cache prune on every
-- 65,536th collection).

count = 70000
chunks = 1000

retained = {}
chunk_source = "local t = { a = 1, b = 2, c = \"label\" } "
    .. "local function f() return t.a + t.b + #t.c end "
    .. "f() "
    .. "retained[#retained + 1] = f"

function verify()
    if #retained ~= chunks then
        error("retained " .. #retained .. " of " .. chunks .. " chunks")
    end
end
