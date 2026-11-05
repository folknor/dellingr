-- The one script behind [bench.wall], [bench.hotpath] and [bench.alloc]
-- (brokkr.d/bench.toml). Each bench builds the harness (examples/hotpath.rs)
-- in its own feature shape and binds `workload` to one pinned file; this runs
-- the harness on it once per iteration and records the KV report it writes
-- to stderr. The iteration's wall is the whole harness process, the
-- workload's top-level footer included: that footer is the measured
-- workload, not warm-up overhead.

function bench(params, keys)
    local r = binary(params.workload.file):ok()
    for line in string.gmatch(r.stderr, "[^\n]+") do
        local key, value = string.match(line, "^([%w_]+)=(.*)$")
        if key then
            record[key] = tonumber(value) or value
        end
    end
    if record.warm_avg_cost == nil then
        fail("the harness wrote no warm_avg_cost KV to stderr")
    end
    -- The hotpath crate's report, under the instrumented benches.
    if r.stdout ~= "" then
        record.report = r.stdout
        record.report_truncated = r.stdout_truncated
    end
end
