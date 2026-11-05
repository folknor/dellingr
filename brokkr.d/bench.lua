-- The one script behind [bench.wall], [bench.hotpath] and [bench.alloc]
-- (brokkr.d/bench.toml). Each bench builds the harness
-- (src/bin/dellingr-bench.rs) in its own feature shape and binds `workload`
-- to one pinned file; this runs the harness on it once per iteration and
-- records the KV report it writes to stderr. The iteration's wall is the
-- whole harness process, the workload's top-level footer included: that
-- footer is the measured workload, not warm-up overhead.
--
-- Under the instrumented benches the hotpath crate writes its JSON report to
-- the file named here (its env overrides the harness's builder settings),
-- with the metrics server off, as brokkr's typed hotpath capture does. The
-- report lands in `record.hotpath`, which brokkr renders and diffs. A bare
-- build ignores the env and leaves the file empty.

function bench(params, keys)
    local report = output("hotpath.json")
    write(report, "")
    local r = binary_with({
        env = {
            HOTPATH_METRICS_SERVER_OFF = "true",
            HOTPATH_OUTPUT_FORMAT = "json",
            HOTPATH_OUTPUT_PATH = report.path,
        },
        writes = { report },
    }, params.workload.file):ok()
    for line in string.gmatch(r.stderr, "[^\n]+") do
        local key, value = string.match(line, "^([%w_]+)=(.*)$")
        if key then
            record[key] = tonumber(value) or value
        end
    end
    if record.warm_avg_cost == nil then
        fail("the harness wrote no warm_avg_cost KV to stderr")
    end
    if filesize(report) > 0 then
        record.hotpath = json.decode(read(report))
    end
end
