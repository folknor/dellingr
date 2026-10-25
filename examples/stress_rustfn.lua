-- Stress test: many rust_fn calls to see if VM alone causes corruption
print("Starting stress test...")

local count = 0
for i = 1, 10000 do
    -- math.sin is a rust_fn
    local x = math.sin(i)
    local y = math.cos(i)
    local z = math.sqrt(i)
    count = count + 1
end

print("Completed " .. count .. " iterations")
print("PASS")
