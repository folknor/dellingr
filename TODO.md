# Script VM TODO

## Priority 1: Essential

- [ ] Fix function calls with many locals (index out of bounds crash)
- [ ] `math.sin(x)` - sine
- [ ] `math.cos(x)` - cosine
- [ ] `math.atan2(y, x)` - angle from delta
- [ ] `math.sqrt(x)` - square root
- [ ] `math.abs(x)` - absolute value
- [ ] `math.min(a, b, ...)` - minimum
- [ ] `math.max(a, b, ...)` - maximum
- [ ] `math.floor(x)` - round down
- [ ] `math.ceil(x)` - round up
- [ ] `math.random()` / `math.random(n)` / `math.random(m, n)` - random numbers
- [ ] `math.pi` - constant
- [ ] `#table` - table length (currently panics)
- [ ] `break` statement - exit loops early
- [ ] Generic for loops - `for k, v in pairs(t)` / `for i, v in ipairs(t)`

## Priority 2: Important

- [ ] Closures / upvalues - capture outer variables in functions
- [ ] Multiple return values - `local x, y = get_position()`
- [ ] `tonumber(s)` - string to number
- [ ] `tostring(x)` - value to string

## Priority 3: Nice to Have

- [ ] `string.sub(s, i, j)` - substring
- [ ] `string.find(s, pattern)` - pattern matching
- [ ] `string.format(fmt, ...)` - formatted output
- [ ] `table.insert(t, v)` - append to array
- [ ] `table.remove(t, i)` - remove from array
- [ ] `table.sort(t)` - sort array
- [ ] Method call syntax - `obj:method()` → `obj.method(obj)`
- [ ] Varargs - `function f(...)`

## Won't Implement

- Metatables
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
- [x] Tables: creation, field access, index access
- [x] Functions: definition, calls, single return
- [x] Local/global variables
- [x] Instruction counting with limits
- [x] `print(...)`
- [x] `type(val)`
- [x] `ipairs(t)` (iterator only, generic for not working)
- [x] `unpack(t)`
