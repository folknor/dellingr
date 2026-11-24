-- Harness scenario: snapshot_load
-- `State::load_state` of this build's own save of the world graph built
-- below, dropping each restored State (teardown included); a codec change
-- that changes the save changes the input too. Needs the snapshot build.

count = 1500

world = { tick = 0, units = {}, names = {} }
for i = 1, 2000 do
    world.units[i] = {
        id = i,
        x = i * 1.5,
        y = i % 37,
        hp = 100 - i % 50,
        kind = i % 3 == 0 and "scout" or "tank",
        tags = { i % 7, i % 11 },
    }
    world.names["unit_" .. i] = i
end
function world.step(dt)
    world.tick = world.tick + dt
end

function verify()
    local u = world.units[2000]
    if u == nil or u.id ~= 2000 or world.names.unit_1999 ~= 1999 then
        error("the restored world graph is incomplete")
    end
end
