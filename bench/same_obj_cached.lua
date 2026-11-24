-- brokkr verdict workload derived from examples/fields/same_obj_cached.lua (see
-- that file for what it measures). Same kernel, repeated K times per _bench
-- call so one call is ~100ms and the standalone run takes a few seconds in a
-- release build.

local entity = {
    pos_x = 1.5,
    pos_y = 2.5,
    pos_z = 3.5,
    vel_x = 0.1,
    vel_y = 0.2,
    vel_z = 0.3,
}

local function kernel()
    local px = entity.pos_x
    local py = entity.pos_y
    local pz = entity.pos_z
    local vx = entity.vel_x
    local vy = entity.vel_y
    local vz = entity.vel_z
    local sum = 0
    for i = 1, 1000 do
        sum = sum + px + py + pz
        sum = sum + vx + vy + vz
    end
    return sum
end

local K = 7500

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/same_obj_cached: true")
