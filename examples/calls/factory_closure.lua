-- Pins the RuntimeCaches-per-closure regression vs v0.1.0. Pre-split,
-- Rc<Chunk> carried caches inline so all closures of the same chunk
-- shared their field/global/method caches. Post-split, alloc_lua_fn
-- allocates a fresh Arc<RuntimeCaches> per closure, so factory
-- patterns - mk() returning a new closure each call - pay cold-cache
-- cost on every produced closure's first access. This bench measures
-- exactly that pattern.

local function mk()
    return function(t)
        return t.x + t.y
    end
end

function _bench()
    local t = { x = 1, y = 2 }
    local sum = 0
    for i = 1, 1000 do
        local f = mk()
        sum = sum + f(t)
    end
    return sum
end

for i = 1, 1500 do _bench() end
print("calls/factory_closure: true")
