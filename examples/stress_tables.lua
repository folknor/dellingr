-- Stress test: many table creations/destructions
print("Starting table stress test...")

for i = 1, 5000 do
    local t = {}
    t.x = i
    t.y = i * 2
    t.data = {}
    t.data.a = 1
    t.data.b = 2
end

print("Phase 1 done")

local kept = {}
for i = 1, 1000 do
    kept[i] = {}
    kept[i].id = i
end

print("Phase 2 done")
print("PASS")
