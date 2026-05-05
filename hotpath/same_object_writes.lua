-- Probe: hot field writes on a single object.
-- Mirror of same_object_fields but for writes. A per-callsite SET_FIELD
-- inline cache keyed on (table identity, index) would hit every
-- iteration here. The arithmetic baseline can be computed by comparing
-- against same_object_fields (same shape, reads instead of writes).

local entity = {
    pos_x = 1.5,
    pos_y = 2.5,
    pos_z = 3.5,
    vel_x = 0.1,
    vel_y = 0.2,
    vel_z = 0.3,
}

function _bench()
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
