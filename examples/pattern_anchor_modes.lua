-- A leading `^` anchors find, match and gsub, and is a literal byte for
-- gmatch. The same pattern string is used by every function in turn, so a
-- shared compiled-pattern cache must keep the two readings apart. Empty
-- patterns match the empty string at every allowed position.

local subject = "^a^a"
local caret = "^a"

local s, e = string.find(subject, caret)
print("find anchors a leading caret: " .. tostring(s == nil and e == nil))

local found = {}
for w in string.gmatch(subject, caret) do found[#found + 1] = w end
print("gmatch reads a leading caret literally: "
    .. tostring(#found == 2 and found[1] == "^a" and found[2] == "^a"))

print("match anchors after gmatch: " .. tostring(string.match("ab", caret) == "a"))

local replaced, n = string.gsub("aaa", caret, "x")
print("gsub anchors to one replacement: " .. tostring(replaced == "xaa" and n == 1))

local carets = 0
for w in string.gmatch("^^x^", "^*") do
    if w ~= "" then carets = carets + #w end
end
print("gmatch repeats a literal caret: " .. tostring(carets == 3))

local lone = {}
for w in string.gmatch("a^b", "^") do lone[#lone + 1] = w end
print("gmatch lone caret: " .. tostring(#lone == 1 and lone[1] == "^"))

local fs, fe = string.find("abc", "", 4)
local gone = string.find("abc", "", 5)
local ps, pe = string.find("abc", "", 2, true)
print("find empty pattern: "
    .. tostring(fs == 4 and fe == 3 and gone == nil and ps == 2 and pe == 1))

print("match empty pattern: " .. tostring(string.match("abc", "", 3) == ""
    and string.match("abc", "", 5) == nil and string.match("", "") == ""))

local as, ae = string.find("abc", "^", 2)
print("find bare anchor: " .. tostring(as == 2 and ae == 1))

local empty_gsub, empty_count = string.gsub("abc", "", "-")
local limited, limited_count = string.gsub("abc", "", "-", 2)
print("gsub empty pattern: " .. tostring(empty_gsub == "-a-b-c-" and empty_count == 4
    and limited == "-a-bc" and limited_count == 2))

local boundaries = 0
for w in string.gmatch("abc", "") do
    if w == "" then boundaries = boundaries + 1 end
end
print("gmatch empty pattern: " .. tostring(boundaries == 4))
