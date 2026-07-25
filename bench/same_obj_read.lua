-- brokkr verdict workload derived from examples/fields/same_obj_read.lua
-- (see that file for the probe rationale). Same kernel, repeated K times
-- per _bench call so one call is ~100ms and the standalone run takes a
-- few seconds in a release build.

local entity = {
    pos_x = 1.5,
    pos_y = 2.5,
    pos_z = 3.5,
    vel_x = 0.1,
    vel_y = 0.2,
    vel_z = 0.3,
}

local function kernel()
    local sum = 0
    for i = 1, 1000 do
        sum = sum + entity.pos_x + entity.pos_y + entity.pos_z
        sum = sum + entity.vel_x + entity.vel_y + entity.vel_z
    end
    return sum
end

local K = 700

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/same_obj_read: true")
