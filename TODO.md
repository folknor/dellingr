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

### Cost System Gaps
- [ ] String concatenation is free but allocates memory - consider charging based on output length
- [ ] `SetList(0)` charges 1 regardless of actual element count - should charge after computing count
- [ ] `table.sort` uses O(n²) bubble sort but only charges once

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

## Code Review Findings

Findings from expert code review (January 2026). Organized by priority.

### HIGH PRIORITY - Correctness Issues

- [x] **Unsound `Eq` implementation for Val** (`lua_val.rs` `impl Hash for Val`)
  - f64 doesn't implement Eq, but Val does - this violates HashMap invariants
  - Two NaN values compare unequal but may hash to same bucket
  - `debug_assert!` in `Hash for Val` only fires in debug builds
  - **Fixed:** Changed `debug_assert!` to `assert!` in release builds

- [x] **Non-deterministic table iteration** (`table.rs` `Table::next()`)
  - `HashMap::iter()` order is not guaranteed
  - `pairs(t)` returns elements in arbitrary order that can change between runs
  - **Fixed:** Replaced HashMap with IndexMap to maintain insertion order

- [x] **No metamethod recursion limit** (`vm.rs` `handle_index_metamethod`, `handle_newindex_metamethod`)
  - `__index` and `__newindex` chains have no depth limit
  - A table whose `__index` is itself causes stack overflow
  - **Fixed:** Added `metamethod_depth` counter with MAX_METAMETHOD_DEPTH=200

- [x] **Panic on invalid stack index** (`vm.rs` `convert_idx()`)
  - `convert_idx()` panics on out-of-bounds, called from public API methods
  - **Fixed:** Changed `convert_idx()` to return `Result<usize, Error>` with InvalidStackIndex error

- [ ] **GC runs synchronously during allocation** (`object.rs` `GcHeap::new_obj_from_raw()`)
  - GC pause time is O(n) where n is heap size
  - Single `NewTable` could trigger full collection
  - **Fix:** Consider incremental marking, pre-allocation pools, or expose `gc_step()` for host-controlled collection

### MEDIUM PRIORITY - Correctness Issues

- [x] **Integer overflow in jump** (`frame.rs` `Frame::jump()`)
  - `wrapping_add` with negative offset cast to usize wraps incorrectly
  - Negative offset larger than `ip` jumps to massive address
  - **Fixed:** Changed to checked arithmetic with InvalidJump error

- [x] **Infinite loop if for-step is 0** (`frame.rs` `check_numeric_for_condition()`)
  - `check_numeric_for_condition` doesn't check for zero step
  - Loop becomes infinite (condition always true, var never changes)
  - **Fixed:** Added check for step == 0.0 that skips loop body

- [x] **Panic in public `concat()` API** (`vm.rs` `State::concat()`)
  - Uses `assert!(n == 2, ...)` instead of returning error
  - **Fixed:** Returns proper ArgError for n < 2

- [x] **No stack/call depth limits**
  - Neither Rust stack (recursion) nor Lua stack (Vec growth) are bounded
  - Malicious script could cause stack overflow or memory exhaustion
  - **Fixed:** Added `call_depth` counter with MAX_CALL_DEPTH=1000 and stack size check with MAX_STACK_SIZE=1_000_000

- [ ] **O(n²) upvalue closing** (`vm.rs` `State::close_upvalues()`)
  - `Vec::remove(0)` is O(n), called in a loop
  - **Fix:** Use `VecDeque` or reverse sort order and pop from end

### HIGH PRIORITY - Performance Optimizations

- [x] **Batch cost checking** (`frame.rs` `Frame::eval()`)
  - Accumulated cost locally, flushed every 64 ops
  - Reduces per-operation overhead by avoiding budget check on every costed op

- [ ] **Replace GC linked list with arena** (`object.rs` `GcHeap` struct)
  - Linked list traversal is cache-hostile (each node is separate allocation)
  - **Fix:** Use contiguous `Vec<Option<WrappedObject>>` with free list

- [x] **Cache table `array_len`** (`table.rs` `Table::array_len()`)
  - Added `Cell<Option<usize>>` cache, invalidated on integer key changes
  - `array_insert`/`array_remove`/`set_array` update cache directly with known length

- [ ] **Fixed-width 32-bit instruction encoding** (`instr.rs` `Instr` enum)
  - Current instructions are ~16 bytes each (enum with isize variant)
  - 1000-instruction function = 16KB, may not fit L1 cache
  - **Fix:** Use 32-bit encoding `[opcode:8][A:8][B:8][C:8]` (4x smaller)

### MEDIUM PRIORITY - Performance Optimizations

- [ ] **Add `#[inline(always)]` to hot paths**
  - `State::instr_get_local()`, `State::instr_set_local()`, `State::pop_val()`
  - `State::eval_float_float()`, `State::eval_float_bool()`
  - `State::consume_cost()` (at minimum the fast path)

- [ ] **Avoid cloning `Chunk` in closures** (`object.rs` `ObjectPtr::as_lua_function()`, `frame.rs` `get_nested_chunk()`)
  - Every closure creation clones entire chunk including string literals
  - **Fix:** Use `Rc<Chunk>` to share chunk data between instances

- [ ] **Replace `Rc<RefCell<Upvalue>>` with arena** (`vm.rs` `State::find_or_create_upvalue()`)
  - Three allocations per upvalue + atomic refcounting on every clone
  - Lifetime is well-defined: closed when owning frame returns
  - **Fix:** Arena-allocated pool with indices instead of Rc pointers

- [ ] **Pre-size stack vector** (`vm.rs` `State::empty()`)
  ```rust
  stack: Vec::with_capacity(256),  // Typical function depth * locals
  ```

- [ ] **Small-table optimization** (`table.rs` `Table` struct)
  - HashMap overhead is high for 1-4 entry tables
  - **Fix:** Inline small arrays with linear scan:
  ```rust
  enum TableStorage {
      Inline([(Val, Val); 4], u8),  // len field
      Map(HashMap<Val, Val>),
  }
  ```

- [ ] **String interning overhead** (`object.rs` `GcHeap::new_string()`)
  - Every allocation goes through HashSet lookup
  - `new_string` takes closure for marking (function call overhead)
  - **Fix:** Dedicated string arena with inline hash buckets

### LOW PRIORITY - Performance Optimizations

- [ ] **NaN-boxing for Val** (`lua_val.rs` `Val` enum)
  - Current Val is ~16-24 bytes, every stack op copies this
  - **Fix:** Pack all values into 64-bit float with tag bits in NaN space

- [ ] **Superinstructions for common patterns**
  - `GetLocal` + `GetField` (method dispatch)
  - `PushNum` + `Add` (constant arithmetic)
  - `Call(1, 1)` (single-arg single-return calls)

- [ ] **Pointer tagging for Val**
  - If allocations are 8-byte aligned, use low bits for type tags

- [ ] **Fixed-size array for well-known globals**
  - `print`, `pairs`, `ipairs`, `type`, etc. accessed frequently
  - String interning for global names enables pointer comparison

- [ ] **Use `#[cfg(feature = "debug_vm")]` instead of `option_env!`** (`frame.rs` `Frame::eval()` debug print)
  - Current debug print may pull in format machinery even when disabled

### API Improvements

- [ ] **Replace magic 255 values with proper types** (`vm.rs` `State::call()`, `instr.rs` `Instr::Return`)
  - `num_args == 255`: vararg call base
  - `num_ret_expected == 255`: return all
  - `Return(255)`: return all
  - **Fix:** Use `Option<u8>` or dedicated enum

- [ ] **Expose GC control to host** (`object.rs` `GcHeap`)
  - `gc_step()` for incremental collection
  - `gc_collect()` for explicit full collection
  - Allow hosts to run GC at known safe points

### Code Organization

- [x] **Split `vm.rs` (~1300 lines)**
  - Extracted metamethod handling to `vm/metamethod.rs`
  - Extracted stack operations to `vm/stack.rs`
  - Extracted table operations to `vm/table_ops.rs`
  - Extracted evaluation/call logic to `vm/eval.rs`
  - Core vm.rs now ~200 lines (struct, constructors, globals, cost budget)

- [ ] **Improve test coverage**
  - Metamethod edge cases
  - Upvalue stress tests
  - GC stress scenarios
  - Error recovery paths

## Development Guidelines

### Error Handling in RustFunc

As of commit efc1da2, most VM stack operations return `Result` instead of panicking. When writing a `RustFunc` (a Rust function callable from Lua), you must propagate errors:

```rust
// CORRECT - propagate errors with ?
add_fn!("example", |state| {
    state.check_type(1, LuaType::Table)?;
    let val = state.to_string(2)?;
    state.remove(1)?;
    state.push_value(1)?;
    Ok(1)
});

// WRONG - these will fail to compile
add_fn!("broken", |state| {
    state.remove(1);  // Error: returns Result, not ()
    Ok(0)
});
```

**Methods that return `Result` (partial list):**
- Stack access: `remove()`, `insert()`, `replace()`, `push_value()`, `copy_val()`
- Table ops: `get_table()`, `set_table_raw()`, `get_metatable_of()`, `set_metatable_of()`
- Type conversion: `to_string()`, `to_number()`
- Validation: `check_type()`

**Methods that are infallible:**
- `push_nil()`, `push_number()`, `push_boolean()`, `push_string()`, `push_rust_fn()`
- `pop()`, `set_top()`, `get_top()`
- `new_table()`, `set_global()`, `get_global()`

### Positive Highlights (from review)

- Clean architecture with good separation between compiler, VM, and stdlib
- Cost system philosophy (control flow free, charge for work) is game-appropriate
- Error handling structure is well-designed with good `From` implementations
- Upvalue open/closed mechanism is textbook-correct
- Unsafe surface is small and well-contained in `object.rs`
- Lint configuration shows attention to code quality

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
