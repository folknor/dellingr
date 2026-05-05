-- Probe: hot field reads on a single object.
-- Unlike fields/polymorphic.lua (which iterates over a fresh table each step),
-- this bench accesses the same table repeatedly. A per-call-site inline cache
-- keyed on (table identity, field name) -> index hits every iteration here.
-- Compare directly against fields/same_obj_cached.lua, which hoists the field
-- reads into locals once per outer iteration.

local entity = {
    pos_x = 1.5,
    pos_y = 2.5,
    pos_z = 3.5,
    vel_x = 0.1,
    vel_y = 0.2,
    vel_z = 0.3,
}

function _bench()
    local sum = 0
    for i = 1, 1000 do
        sum = sum + entity.pos_x + entity.pos_y + entity.pos_z
        sum = sum + entity.vel_x + entity.vel_y + entity.vel_z
    end
    return sum
end

for i = 1, 800 do _bench() end
print("fields/same_obj_read: true")
