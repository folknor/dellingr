local t = {10, 20, 30, 40, 50}
print(#t)  -- should print 5

local empty = {}
print(#empty)  -- should print 0

local sparse = {1, 2, nil, 4}
print(#sparse)  -- should print 2 (stops at first nil)
