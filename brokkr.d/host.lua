-- The one script behind [bench.host], [bench.host_kernel], [bench.snapshot]
-- and [bench.snapshot_kernel] (brokkr.d/bench.toml): Rust-driven scenarios.
-- Each bench builds the harness (src/bin/dellingr-bench.rs) and binds
-- `scenario` to one pinned file under bench/host/, whose
-- `-- Harness scenario: <kind>` line selects the driver and whose `count`
-- sets the scale. This runs the harness on it once per iteration, records
-- the KV report it writes to stderr, and returns its success line as the
-- verdict.
--
-- The stored wall is the whole harness process: the file's setup, the
-- driver's `count` operations (the WARM phase) and validation. `measured_us`
-- in the record is the driver loop alone; read it, or the sidecar's WARM
-- phase, when attributing a change to the operation itself.
--
-- A binary older than the scenario harness, or one that does not know the
-- file's kind, exits nonzero, so a mismatched baseline fails its run and
-- never pairs.

function bench(params, keys)
    local r = binary_with({ hotpath = "harness" }, params.scenario.file):ok()
    for line in string.gmatch(r.stderr, "[^\n]+") do
        local key, value = string.match(line, "^([%w_]+)=(.*)$")
        if key then
            record[key] = tonumber(value) or value
        end
    end
    if record.measured_us == nil or record.count == nil then
        fail("the harness wrote no scenario KVs to stderr")
    end
    local verdict = string.match(r.stdout, "([^\n]*)\n?$")
    local count = verdict and string.match(verdict, "^host/[%w_]+: (%d+): true$")
    if count == nil then
        fail("the scenario printed no success line")
    end
    if tonumber(count) ~= record.count then
        fail("the success line's count disagrees with the count KV")
    end
    return verdict
end
