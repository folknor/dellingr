-- The metatable shared by all strings (reference Lua semantics).
local mt = getmetatable("")
print("string metatable is a table: " .. tostring(type(mt) == "table"))
print("__index is the string library: " .. tostring(mt.__index == string))
print("shared by every string: " .. tostring(getmetatable("abc") == mt))

local function up(s) return s:upper() end
print("method call: " .. tostring(up("a") == "A"))

local saved = string
string = nil
print("string = nil keeps methods: " .. tostring(up("b") == "B"))
string = { upper = function() return "rebound" end }
print("rebinding string is ignored: " .. tostring(up("c") == "C"))
string = saved

mt.__index = { upper = function(s) return "custom " .. s end }
print("__index table redirects: " .. tostring(up("d") == "custom d"))
mt.__index = function(s, k) return s .. ":" .. k end
print("__index function gets receiver and key: " .. tostring(("e").f == "e:f"))
mt.__index = saved
print("restored: " .. tostring(up("g") == "G"))
