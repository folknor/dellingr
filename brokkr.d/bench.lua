-- The one script behind [bench.wall], [bench.kernel] and [bench.alloc]
-- (brokkr.d/bench.toml). Each bench builds the harness
-- (src/bin/dellingr-bench.rs) and binds `workload` to one pinned file; this
-- runs the harness on it once per iteration and records the KV report it
-- writes to stderr. The iteration's wall is the whole harness process, the
-- workload's top-level footer included: that footer is the measured
-- workload, not warm-up overhead.
--
-- The call is marked as the `harness` hotpath stage, so brokkr points the
-- hotpath crate's JSON report at a file of its own and turns the metrics
-- server off. Under `brokkr bench --hotpath` the instrumented build must
-- capture it; a bare build leaves it missing, which is evidence only.

function bench(params, keys)
    local r = binary_with({ hotpath = "harness" }, params.workload.file):ok()
    for line in string.gmatch(r.stderr, "[^\n]+") do
        local key, value = string.match(line, "^([%w_]+)=(.*)$")
        if key then
            record[key] = tonumber(value) or value
        end
    end
    if record.warm_avg_cost == nil then
        fail("the harness wrote no warm_avg_cost KV to stderr")
    end
end
