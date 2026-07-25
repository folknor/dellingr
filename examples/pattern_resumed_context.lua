-- Regression coverage for resumed Lua-pattern searches retaining subject context.
local a, b = string.find("ab", "%f[%a]%a", 2)
local replaced, count = string.gsub("abcd", "%f[%w]%w", "X")
local final
for value in string.gmatch("ab", ".%f[%z]") do
    final = value
end

print("resumed frontier: " .. tostring(a == nil and b == nil))
print("resumed gsub: " .. tostring(replaced == "Xbcd" and count == 1))
print("resumed gmatch: " .. tostring(final == "b"))
