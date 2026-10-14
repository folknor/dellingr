-- DIFF: zero_for_step - dellingr skips a numeric for with a zero step in both
-- directions. Lua 5.4 raises "'for' step is zero"; Lua 5.2 skips an ascending
-- range but runs a descending one forever (the break guard below bounds it).
-- See docs/compatibility.md.

local function count(start, limit, step)
    local n = 0
    for _ = start, limit, step do
        n = n + 1
        if n > 3 then
            break
        end
    end
    return n
end

print("ascending literal zero step skips: " .. tostring(count(1, 10, 0) == 0))
print("descending literal zero step skips: " .. tostring(count(10, 1, 0) == 0))
print("equal bounds zero step skips: " .. tostring(count(5, 5, 0) == 0))
print("float zero step skips: " .. tostring(count(1.5, 10, 0.0) == 0))
print("negative zero step skips: " .. tostring(count(1, 10, -0.0) == 0))

local ran = false
for _ = 1, 10, 0 do
    ran = true
    break
end
print("inline zero step body never runs: " .. tostring(not ran))
