# Script VM TODO

## Priority 2: Useful Additions

### Global Functions
- [ ] `_G` - global environment table

### Math Library
- [ ] `math.tan(x)` - tangent
- [ ] `math.acos(x)`, `math.asin(x)` - inverse trig
- [ ] `math.atan(y [, x])` - Lua 5.3+ style (replaces atan2)
- [ ] `math.deg(x)`, `math.rad(x)` - angle conversion
- [ ] `math.exp(x)` - e^x
- [ ] `math.log(x [, base])` - logarithm with optional base
- [ ] `math.fmod(x, y)` - float modulo
- [ ] `math.modf(x)` - returns integer and fractional parts
- [ ] `math.huge` - infinity constant

### String Library
- [ ] `string.byte(s [, i [, j]])` - get byte values of characters
- [ ] `string.char(...)` - create string from byte values

## Priority 3: Nice to Have

### Global Functions
- [ ] `rawlen(v)` - length without metamethods (Lua 5.2+). This is kind of silly, we should just make #tbl return this size by default if we add metatables
- [ ] `require(modname)` - module system, but we only allow to require files already loaded by us, we need to define this system properly with regards to mods and such

### Table Library
- [ ] `table.move(a1, f, e, t [, a2])` - move elements (Lua 5.3+)

## Known Limitations

### Closures: Capture-by-Value
Upvalues are captured by value (copy) rather than by reference. This means:
- Modifications to captured variables inside a closure don't affect the original
- Multiple closures capturing the same variable get independent copies
- Counter patterns like `local count = 0; return function() count = count + 1; return count end` won't work as expected (each call returns 1)

Full Lua semantics would require "open upvalues" that point to stack slots and get "closed" to heap storage when the enclosing function returns.

### Multiple Returns/Varargs as Function Arguments
Using multiple return values or varargs directly as function arguments doesn't work:
```lua
local add = function(a, b) return a + b end
local vals = function() return 3, 4 end
add(vals())  -- Won't work: only first return value is passed

local f = function(...)
    print(...)  -- Won't work: only first vararg is passed
end
```
Workaround: capture values first, then pass them:
```lua
local a, b = vals()
add(a, b)  -- Works correctly

local a, b = ...
print(a, b)  -- Works correctly
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
- [x] Closures/upvalues (capture-by-value semantics)
- [x] Multiple return values (including empty return, chained returns)
- [x] Method call syntax (`obj:method(args)`)
- [x] Varargs (`...` in function parameters and body)
- [x] Table library: `table.insert`, `table.remove`, `table.sort`, `table.unpack`
- [x] String library: `string.sub`, `string.find`, `string.format`, `string.len`, `string.upper`, `string.lower`, `string.rep`, `string.reverse`
- [x] Metatables: `setmetatable`, `getmetatable`
- [x] Metamethods: `__index`, `__newindex`, `__tostring`, `__call`, `__len`
- [x] String methods: `str:upper()` syntax via implicit string metatable
- [x] `string.match`, `string.gmatch`, `string.gsub` - pattern matching
- [x] `table.concat` - join array elements with separator
- [x] `select(index, ...)` and `select('#', ...)` - vararg selection
- [x] `rawget`, `rawset` - table access bypassing metamethods
- [x] `table.pack(...)` - returns `{..., n=count}` with explicit length field
- [x] `{...}` syntax - collect varargs into table
