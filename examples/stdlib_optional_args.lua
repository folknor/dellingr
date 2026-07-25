local function count(...)
    return select("#", ...)
end

local function equal(actual, expected)
    return actual == expected
end

local find_i, find_j = ("a.b"):find(".", nil, true)
local sub = string.sub("abc", 2, nil)
local matched = string.match("abc", "b", nil)
local gsubbed, replacements = string.gsub("aaa", "a", "b", nil)
print("optional string arguments: " .. tostring(
    equal(find_i, 2) and equal(find_j, 2) and sub == "bc" and matched == "b"
        and gsubbed == "bbb" and replacements == 3
))

local concat_defaults = table.concat({ "a", "b" }, nil, nil, nil)
local sortable = { 2, 1 }
table.sort(sortable, nil)
print("optional table arguments: " .. tostring(
    concat_defaults == "ab" and sortable[1] == 1 and sortable[2] == 2
))

local values = { 10, 20 }
local table_default_count = count(table.unpack(values, nil, nil))
local compat_unpack = unpack or table.unpack
local global_default_count = count(compat_unpack(values, nil, nil))
print("optional unpack endpoints: " .. tostring(
    table_default_count == 2 and global_default_count == 2
))

local sparse = {}
sparse[2] = "x"
sparse[-1] = "n"
sparse[0] = "z"
print("explicit concat ranges: " .. tostring(
    table.concat(sparse, "", 2, 2) == "x" and table.concat(sparse, "", -1, 0) == "nz"
))

local unpacked = { [-2] = "a", [-1] = "b", [0] = "c", [1] = "d", [2] = "e" }
local table_negative = table.concat({ table.unpack(unpacked, -2, 2) }, "")
local global_negative = table.concat({ compat_unpack(unpacked, -2, 2) }, "")
print("negative unpack ranges: " .. tostring(table_negative == "abcde" and global_negative == "abcde"))
