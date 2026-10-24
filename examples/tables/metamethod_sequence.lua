-- The table library follows Lua 5.4's metamethod rules: lengths come from
-- __len, reads go through __index and writes through __newindex.

local function proxy(...)
  local store = {...}
  local log = {}
  local p = setmetatable({}, {
    __len = function() return #store end,
    __index = function(_, k) return store[k] end,
    __newindex = function(_, k, v)
      log[#log + 1] = tostring(k) .. "=" .. tostring(v)
      store[k] = v
    end,
  })
  return p, store, log
end

local p, s, log = proxy(10, 20, 30)
local a, b, c = table.unpack(p)
print("unpack honours __len and __index: " .. tostring(a == 10 and b == 20 and c == 30))
print("concat honours __len and __index: " .. tostring(table.concat(p, ",") == "10,20,30"))

p, s, log = proxy(10, 20, 30)
table.insert(p, 40)
print("insert append goes through __newindex: "
  .. tostring(table.concat(log, " ") == "4=40" and rawget(p, 4) == nil and s[4] == 40))

p, s, log = proxy(10, 20, 30)
table.insert(p, 1, 5)
print("insert front shifts through metamethods: "
  .. tostring(table.concat(log, " ") == "4=30 3=20 2=10 1=5"))

p, s, log = proxy(10, 20, 30)
local removed = table.remove(p)
print("remove last: " .. tostring(removed == 30 and table.concat(log, " ") == "3=nil"))

p, s, log = proxy(10, 20, 30)
removed = table.remove(p, 1)
print("remove front: "
  .. tostring(removed == 10 and table.concat(log, " ") == "1=20 2=30 3=nil"))

local src = proxy(10, 20, 30)
local dst, ds, dlog = proxy()
table.move(src, 1, 3, 4, dst)
print("move reads __index, writes __newindex: "
  .. tostring(table.concat(dlog, " ") == "4=10 5=20 6=30" and rawget(dst, 4) == nil))

-- __newindex as a table chain
local sink = {}
local chained = setmetatable({}, {__newindex = setmetatable({}, {__newindex = sink})})
table.insert(chained, "x")
print("insert follows a __newindex table chain: " .. tostring(sink[1] == "x"))

-- A metatable without the sequence events behaves like a plain table.
local plain = setmetatable({1, 2, 3}, {__tostring = function() return "plain" end})
table.insert(plain, 2, 9)
print("metatable without sequence events: " .. tostring(table.concat(plain, ",") == "1,9,2,3"))

-- Explicit bounds skip __len in unpack.
local calls = 0
local counted = setmetatable({1, 2}, {__len = function() calls = calls + 1 return 2 end})
table.unpack(counted, 1, 2)
print("unpack with explicit end skips __len: " .. tostring(calls == 0))
