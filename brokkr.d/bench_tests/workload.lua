local function workload(stdout, stderr)
    return run {
        bench = "wall",
        params = { "--workload", "sort" },
        calls = { { stdout = stdout, stderr = stderr } },
    }
end

local function expect_failure(stdout, stderr, reason)
    local r = workload(stdout, stderr)
    if r.stage ~= "bench" or r.outcome ~= "failed"
        or not string.find(r.reason or "", reason, 1, true) then
        error(r.reason or r.outcome)
    end
end

local verdict = "bench/sort: true"
local kvs = "warm_avg_cost=123\n"

test("wall records the harness report and returns the workload verdict", function()
    local r = workload("setup output\n" .. verdict .. "\n",
        "harness diagnostic\n" .. kvs .. "clock=wall\n")
    if r.outcome ~= "completed" then error(r.reason or r.outcome) end
    if r.verdict ~= verdict then error("wrong workload verdict") end
    if r.record.warm_avg_cost ~= 123 or r.record.clock ~= "wall" then
        error("wrong workload record")
    end
    if r.calls[1].argv[1] ~= project_root .. "/bench/sort.lua"
        or r.calls[1].hotpath ~= "harness" then
        error("wrong workload harness call")
    end
end)

test("wall refuses a report without warm_avg_cost", function()
    expect_failure(verdict .. "\n", "harness diagnostic\n",
        "the harness wrote no warm_avg_cost KV to stderr")
end)

test("wall refuses stdout without a success line", function()
    expect_failure("setup output\n", kvs, "the workload printed no success line")
end)
