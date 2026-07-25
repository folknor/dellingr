-- brokkr verdict workload derived from examples/fields/miss.lua (see that
-- file for the probe rationale: field reads that miss still pay the
-- table-library fallback). Same kernel, repeated K times per _bench call
-- so one call is ~100ms and the standalone run takes a few seconds in a
-- release build.

local cfg = {
    width = 100,
    height = 50,
    depth = 25,
}

local function kernel()
    local n = 0
    for i = 1, 1000 do
        if cfg.optional_scale then n = n + 1 end
        if cfg.optional_tint then n = n + 2 end
        if cfg.optional_label then n = n + 4 end
        n = n + cfg.width + cfg.height
    end
    return n
end

local K = 500

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/miss: true")
