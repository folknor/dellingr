-- Scope-exit upvalue closing. The compiler omits CloseUpvalues for scopes
-- whose locals no closure captures, so each shape below pins a case where the
-- close must survive (or where omitting it must not leak sharing).

-- Captured numeric loop variable: one fresh upvalue per iteration.
local fs = {}
for i = 1, 3 do
    fs[i] = function() return i end
end
print("numeric for fresh per iteration: " .. tostring(fs[1]() == 1 and fs[2]() == 2 and fs[3]() == 3))

-- Captured generic loop variable.
local gs = {}
for k, v in ipairs({10, 20, 30}) do
    gs[k] = function() return v end
end
print("generic for fresh per iteration: " .. tostring(gs[1]() == 10 and gs[3]() == 30))

-- Uncaptured loop in a function that also makes a closure.
local function mixed()
    local sum = 0
    for i = 1, 10 do
        sum = sum + i
    end
    local get = function() return sum end
    return get()
end
print("uncaptured loop beside closure: " .. tostring(mixed() == 55))

-- Sibling scopes reusing the same slot, both captured, then the slot is
-- overwritten by a third local.
local first, second
do
    local a = 1
    first = function() return a end
end
do
    local b = 2
    second = function() return b end
end
local c = 3
print("sibling slot reuse: " .. tostring(first() == 1 and second() == 2 and c == 3))

-- Grandchild captures a grandparent loop local through an intermediate.
local hs = {}
for i = 1, 3 do
    local x = i * 2
    hs[i] = function()
        return function() return x end
    end
end
print("grandchild capture: " .. tostring(hs[1]()() == 2 and hs[3]()() == 6))

-- repeat-until whose condition captures a body local.
local rs = {}
local n = 0
repeat
    n = n + 1
    local y = n
until (function() rs[#rs + 1] = function() return y end return n >= 3 end)()
print("repeat condition capture: " .. tostring(rs[1]() == 1 and rs[2]() == 2 and rs[3]() == 3))

-- A break reached on a later iteration, before a textually later capture.
local ws = {}
local j = 0
while true do
    j = j + 1
    local z = j
    if j == 3 then
        ws[3] = function() return z end
        break
    end
    ws[j] = function() return z end
end
print("break before later capture: " .. tostring(ws[1]() == 1 and ws[2]() == 2 and ws[3]() == 3))

-- Break inside a nested block of a captured loop.
local bs = {}
for i = 1, 5 do
    do
        local w = i
        bs[i] = function() return w end
        if i == 2 then break end
    end
end
print("break in nested block: " .. tostring(bs[1]() == 1 and bs[2]() == 2 and bs[3] == nil))

-- Shared upvalue within one iteration, distinct across iterations.
local incs, gets = {}, {}
for i = 1, 2 do
    local count = 0
    incs[i] = function() count = count + 1 end
    gets[i] = function() return count end
end
incs[1]()
incs[1]()
incs[2]()
print("shared within iteration: " .. tostring(gets[1]() == 2 and gets[2]() == 1))
