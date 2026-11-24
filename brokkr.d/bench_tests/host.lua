local function scenario(stdout, stderr)
    return run {
        bench = "host",
        params = { "--scenario", "state_new" },
        calls = { { stdout = stdout, stderr = stderr } },
    }
end

local function expect_failure(stdout, stderr, reason)
    local r = scenario(stdout, stderr)
    if r.stage ~= "bench" or r.outcome ~= "failed"
        or not string.find(r.reason or "", reason, 1, true) then
        error(r.reason or r.outcome)
    end
end

local verdict = "host/state_new: 42: true"
local kvs = "measured_us=125\ncount=42\n"

test("host records the harness report and returns the scenario verdict", function()
    local r = scenario("setup output\n" .. verdict .. "\n",
        "harness diagnostic\n" .. kvs .. "kind=state_new\n")
    if r.outcome ~= "completed" then error(r.reason or r.outcome) end
    if r.verdict ~= verdict then error("wrong scenario verdict") end
    if r.record.measured_us ~= 125 or r.record.count ~= 42
        or r.record.kind ~= "state_new" then
        error("wrong scenario record")
    end
    if r.calls[1].argv[1] ~= project_root .. "/bench/host/state_new.lua"
        or r.calls[1].hotpath ~= "harness" then
        error("wrong scenario harness call")
    end
end)

test("host refuses a report without measured_us", function()
    expect_failure(verdict .. "\n", "count=42\n",
        "the harness wrote no scenario KVs to stderr")
end)

test("host refuses a report without count", function()
    expect_failure(verdict .. "\n", "measured_us=125\n",
        "the harness wrote no scenario KVs to stderr")
end)

test("host refuses stdout without a success line", function()
    expect_failure("host/state_new: 42: false\n", kvs,
        "the scenario printed no success line")
end)

test("host refuses a verdict count that disagrees with the report", function()
    expect_failure("host/state_new: 41: true\n", kvs,
        "the success line's count disagrees with the count KV")
end)
