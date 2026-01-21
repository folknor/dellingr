# Script VM TODO

## Priority 1: Essential

### Break Statement
- **Files:** `src/compiler/parser.rs`, `src/compiler/token.rs`, `src/instr.rs`, `src/vm/frame.rs`
- **Test:** `examples/loops.lua`
- [ ] Add `Break` token
- [ ] Parse `break` in loops
- [ ] Add `Break` instruction
- [ ] Execute break (jump to loop end)

### Generic For Loops
- **Files:** `src/compiler/parser.rs`, `src/instr.rs`, `src/vm/frame.rs`, `src/lua_std/basic.rs`
- **Test:** `examples/loops.lua`
- [ ] Parse `for k, v in expr do`
- [ ] Add iterator instructions
- [ ] Implement `pairs(t)`
- [ ] Fix `ipairs(t)` to work with generic for

## Priority 2: Important

### Closures / Upvalues
- **Files:** `src/compiler/parser.rs`, `src/compiler.rs` (Chunk), `src/vm/frame.rs`, `src/vm.rs`
- **Test:** `examples/closures.lua`
- [ ] Track upvalue references in compiler
- [ ] Add upvalue storage to Chunk
- [ ] Add `GetUpvalue`/`SetUpvalue` instructions
- [ ] Capture variables at closure creation

### Multiple Return Values
- **Files:** `src/compiler/parser.rs`, `src/vm/frame.rs`, `src/vm.rs`
- **Test:** `examples/functions.lua`
- [ ] Handle multiple returns in compiler
- [ ] Fix return instruction for N values
- [ ] Handle multiple assignment targets

## Priority 3: Nice to Have

### String Library
- **Files:** `src/lua_std/string.rs` (new), `src/lua_std.rs`
- **Test:** `examples/strings.lua`
- [ ] `string.sub(s, i, j)`
- [ ] `string.find(s, pattern)`
- [ ] `string.format(fmt, ...)`

### Table Library
- **Files:** `src/lua_std/table.rs` (new), `src/lua_std.rs`
- **Test:** `examples/tables.lua`
- [ ] `table.insert(t, v)`
- [ ] `table.remove(t, i)`
- [ ] `table.sort(t)`

### Method Call Syntax
- **Files:** `src/compiler/parser.rs`
- **Test:** `examples/methods.lua`
- [ ] Parse `obj:method(args)` as `obj.method(obj, args)`

### Varargs
- **Files:** `src/compiler/parser.rs`, `src/instr.rs`, `src/vm/frame.rs`
- **Test:** `examples/functions.lua`
- [ ] Parse `...` in function params
- [ ] Store varargs in frame
- [ ] Access varargs in function body

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
- [x] Tables: creation, field access, index access, length (`#t`)
- [x] Functions: definition, calls, single return
- [x] Local/global variables
- [x] Instruction counting with limits
- [x] `print(...)`
- [x] `type(val)`
- [x] `tonumber(s)`, `tostring(x)`
- [x] `ipairs(t)` (iterator only, generic for not working)
- [x] `unpack(t)`
- [x] Math library: `sin`, `cos`, `atan2`, `sqrt`, `abs`, `min`, `max`, `floor`, `ceil`, `random`, `pi`
- [x] Bug fix: function calls with many locals
