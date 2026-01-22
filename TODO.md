# Script VM TODO

## Cost

Players write fleets to run in fcomm2, and all fleets get a certain budget per tick to execute code. We don't penalize users for writing good code (clean architecture, many functions), only for actual computational work.

### Budget overflow
Example: each fleet gets 10 points per tick. Normal operations cost 1 point, decisive actions cost 7-10 points. The action that pushes you over budget always completes, then the tick ends.

A savvy player can "squeeze" more value by front-loading calculations:
- Spend 9 points on analysis (headings, trajectories, threat assessment)
- Then commit one 10-point decisive action (spawn, fire, move)
- Total: 19 points of work from a 10-point budget

This rewards thoughtful code organization while ensuring every fleet can do at least one decisive action per tick (can't be starved out of acting).

### Free (no cost)
- Function calls and returns
- Variable declaration and access (local/global)
- Control flow (`if`, `while`, `for`, `repeat`, `break`)
- Logical operators (`and`, `or`, `not`)
- Comparisons (`<`, `>`, `==`, `~=`, `<=`, `>=`)
- Iteration (`pairs`, `ipairs`, `next`, each step)
- Length operator (`#t`, `#str`)
- Table/metatable reads (`t[k]`, `t.field`, `rawget`, `getmetatable`)
- `setmetatable` (but creating the metatable table itself costs)
- `type()`, `select`, `unpack`
- `print`
- All string operations (`..`, `tostring`, `tonumber`, all `string.*` functions)

### Costs (normal)
- Arithmetic (`+`, `-`, `*`, `/`, `%`, `^`)
- All `math.*` functions
- Table writes (`t[k] = v`, `rawset`)
- Table creation (`{}`)
- `table.insert`, `table.remove`, `table.sort`, `table.concat`, `table.move`, `table.pack`
- Game API queries (positions, health, distances, cooldowns, etc.)
- `remote.send` (cost based on message length)

### Decisive actions (high cost / rate-limited)
These actions commit irreversible changes to the game world:
- Spawning a ship
- Firing/launching weapons
- Self-destruct
- Movement commands (move to, orbit, intercept)
- Building/constructing

### Tooling
- [ ] Cost analyzer: CLI tool that takes a Lua file and prints each statement with its cost annotated, so players can optimize their scripts

## Global Functions
- [ ] `require(modname)` - module system, but we only allow to require files already loaded by us, we need to define this system properly with regards to mods and such

## Known Limitations

### Multiple Returns as Function Arguments
Using multiple return values directly as function arguments doesn't fully work:
```lua
local add = function(a, b) return a + b end
local vals = function() return 3, 4 end
add(vals())  -- Won't work: only first return value is passed
```
Workaround: capture values first, then pass them:
```lua
local a, b = vals()
add(a, b)  -- Works correctly
```
Note: Varargs (`...`) as function arguments now works correctly:
```lua
local f = function(...)
    print(...)  -- Works! All varargs are passed
end
```

## Won't Implement

- Integer division (`//`)
- Coroutines
- IO/OS libraries
- Debug library
- pcall/xpcall
- assert
- goto/labels
- Bitwise operators
- `string.rep`, `string.byte`, `string.char` (exploitable for free computation)

## Done

- [x] Basic types: nil, boolean, number, string, table, function
- [x] Arithmetic: `+`, `-`, `*`, `/`, `%`, `^`
- [x] Comparison: `<`, `<=`, `>`, `>=`, `==`, `~=`
- [x] Logical: `and`, `or`, `not`
- [x] Control flow: `if/else/elseif`, `while`, `repeat/until`, numeric `for`
- [x] Tables: creation, field access, index access, length (`#t`)
- [x] Functions: definition, calls, single return
- [x] Local/global variables
- [x] Instruction counting with limits
- [x] `print(...)`
- [x] `type(val)`
- [x] `tonumber(s)`, `tostring(x)`
- [x] `ipairs(t)`, `pairs(t)`, `next(t, k)` with generic for loops
- [x] `unpack(t)`
- [x] Math library: `sin`, `cos`, `atan2`, `sqrt`, `abs`, `min`, `max`, `floor`, `ceil`, `random`, `pi`
- [x] Bug fix: function calls with many locals
- [x] Break statement in loops (`while`, `for`, `repeat`)
- [x] Generic for loops (`for k, v in pairs(t) do`)
- [x] Closures/upvalues with proper capture-by-reference (open/closed upvalues)
- [x] Multiple return values (including empty return, chained returns)
- [x] Varargs as function arguments (`foo(...)` passes all varargs)
- [x] Method call syntax (`obj:method(args)`)
- [x] Varargs (`...` in function parameters and body)
- [x] Table library: `table.insert`, `table.remove`, `table.sort`, `table.unpack`
- [x] String library: `string.sub`, `string.find`, `string.format`, `string.len`, `string.upper`, `string.lower`, `string.reverse`
- [x] Metatables: `setmetatable`, `getmetatable`
- [x] Metamethods: `__index`, `__newindex`, `__tostring`, `__call`, `__len`
- [x] String methods: `str:upper()` syntax via implicit string metatable
- [x] `string.match`, `string.gmatch`, `string.gsub` - pattern matching
- [x] `table.concat` - join array elements with separator
- [x] `select(index, ...)` and `select('#', ...)` - vararg selection
- [x] `rawget`, `rawset` - table access bypassing metamethods
- [x] `table.pack(...)` - returns `{..., n=count}` with explicit length field
- [x] `{...}` syntax - collect varargs into table
- [x] Table methods: `tbl:insert()`, `tbl:concat()` syntax via implicit table lookup
- [x] `_G` - global environment table (proxy with metamethods)
- [x] Math library: `tan`, `acos`, `asin`, `atan`, `deg`, `rad`, `exp`, `log`, `fmod`, `modf`, `huge`
- [x] `table.move` - move/copy elements between tables
