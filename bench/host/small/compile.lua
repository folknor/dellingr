-- Harness scenario: compile
-- `Program::compile` of one generated source, dropping each Program: parse,
-- codegen and verification without `State::load`. The source is built below
-- (240 global functions of mixed statements, under the nested-function cap),
-- so its size is pinned here.

count = 10

local parts = {}
for i = 1, 240 do
    parts[#parts + 1] = "function f" .. i .. "(a, b, t)\n"
        .. "    local x = a * " .. i .. " + b\n"
        .. "    if x > 10 then x = x - t.k else x = x + 1 end\n"
    for r = 1, 6 do
        parts[#parts + 1] = "    for j = 1, " .. r .. " do x = x + j * 0.5 end\n"
            .. "    local y" .. r .. " = { a = x, b = { x, x * 2 } }\n"
            .. "    x = x + y" .. r .. ".a - y" .. r .. ".b[2] / 3\n"
    end
    parts[#parts + 1] = "    t.v" .. i .. " = { x, b, \"s" .. i .. "\" }\n"
        .. "    return math.floor(x) + #t.v" .. i .. "\n"
        .. "end\n"
end
source = table.concat(parts)
