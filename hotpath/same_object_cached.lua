-- Sibling of same_object_fields.lua: same arithmetic, but the six fields
-- are pre-loaded into locals once. The delta isolates per-call-site
-- field-lookup overhead.

local entity = {
    pos_x = 1.5,
    pos_y = 2.5,
    pos_z = 3.5,
    vel_x = 0.1,
    vel_y = 0.2,
    vel_z = 0.3,
}

function _bench()
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
