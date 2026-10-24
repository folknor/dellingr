local cases = {
    {"[z-a]", "", "za-5q"},
    {"[%a-z]", "za-q", "5"},
    {"[%d-a]", "a-5", "zq"},
    {"[%--a]", "a-", "z5q"},
}

local function classify(pattern, bytes)
    local result = ""
    for i = 1, #bytes do
        local byte = string.sub(bytes, i, i)
        if i > 1 then result = result .. "," end
        local matched = string.find(byte, "^" .. pattern .. "$") ~= nil
        result = result .. byte .. "=" .. tostring(matched)
    end
    return result
end

for _, case in ipairs(cases) do
    local pattern, members, nonmembers = case[1], case[2], case[3]
    print(pattern .. " members=" .. classify(pattern, members)
        .. " nonmembers=" .. classify(pattern, nonmembers))
end

local star_pattern = ""
local lazy_pattern = ""
for i = 1, 200 do
    star_pattern = star_pattern .. "b*"
    lazy_pattern = lazy_pattern .. "b-"
end

local star_start, star_end = string.find("", star_pattern)
local lazy_start, lazy_end = string.find("", lazy_pattern)
print("empty repeats=" .. star_start .. "," .. star_end
    .. "|" .. lazy_start .. "," .. lazy_end)
