-- brokkr verdict workload derived from examples/fields/same_obj_write.lua (see
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
    for i = 1, 1000 do
        entity.pos_x = i
        entity.pos_y = i + 1
        entity.pos_z = i + 2
        entity.vel_x = i + 3
        entity.vel_y = i + 4
        entity.vel_z = i + 5
    end
    return entity.pos_x + entity.vel_z
end

local K = 1800

function _bench()
    local acc = 0
    for _ = 1, K do
        acc = acc + kernel()
    end
    return acc
end

for i = 1, 30 do _bench() end
print("bench/same_obj_write: true")
