-- The `#` operator on a table returns a "border" - any non-negative integer N
-- where t[N] is non-nil (or N == 0) and t[N+1] is nil. For sequences (no holes)
-- this is the length. For non-sequences any valid border is acceptable, so
-- non-sequence cases assert only the border invariant.

local function is_border(t, n)
    if n == 0 then return t[1] == nil end
    return t[n] ~= nil and t[n + 1] == nil
end

-- Empty.
local t = {}
print("empty: " .. tostring(#t == 0))

-- Dense, inline-sized.
t = {}
for i = 1, 4 do t[i] = i end
print("dense_4: " .. tostring(#t == 4))

-- Dense, post-promotion to map storage.
t = {}
for i = 1, 100 do t[i] = i end
print("dense_100: " .. tostring(#t == 100))

-- Boundary of exponential doubling: 8 lands on hi=16 which is nil.
t = {}
for i = 1, 8 do t[i] = i end
print("dense_8: " .. tostring(#t == 8))

-- Power of two minus one.
t = {}
for i = 1, 7 do t[i] = i end
print("dense_7: " .. tostring(#t == 7))

-- Single element.
t = {}
t[1] = "x"
print("single: " .. tostring(#t == 1))

-- Border invariant: hole somewhere in the middle of a dense run.
t = {}
for i = 1, 200 do t[i] = i end
t[100] = nil
print("hole_invariant: " .. tostring(is_border(t, #t)))

-- Border invariant: two dense runs with a gap.
t = {}
for i = 1, 5 do t[i] = i end
for i = 10, 15 do t[i] = i end
print("two_runs_invariant: " .. tostring(is_border(t, #t)))

-- Border invariant: no t[1].
t = {}
t[2] = "a"
t[3] = "b"
print("no_first_invariant: " .. tostring(is_border(t, #t)))
