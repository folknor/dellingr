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
- [x] Cost analyzer: CLI `--analyze` flag shows static cost breakdown by operation type

### Cost System Gaps (needs discussion before implementation)
- [ ] `SetList(0)` charges 1 regardless of actual element count - should charge after computing count
- [ ] `table.sort` uses O(n²) bubble sort but only charges once

## Global Functions
- [ ] `require(modname)` - module system, but we only allow to require files already loaded by us, we need to define this system properly with regards to mods and such

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

- [x] **Host-controlled GC** (`vm.rs` `State`)
  - GC pause time is O(n) where n is heap size
  - Single `NewTable` could trigger full collection mid-script
  - **Fixed:** Added host-controlled GC API:
    - `gc_disable_auto()` - sets threshold to usize::MAX, disabling auto-GC
    - `gc_set_threshold(n)` - set custom threshold
    - `gc_should_run()` - check if threshold exceeded
    - `gc_collect()` - manually trigger full collection
    - `object_count()`, `string_count()`, `heap_size()` - memory tracking
  - fcomm2 can now control when GC runs (e.g., between ticks)
  - Incremental marking not implemented - full collection still O(n)

- [ ] **GC tuning needs real fleet data**
  - Current threshold: starts at 20, doubles after each collection (`max(survivors * 2, 20)`)
  - Triggers: `new_table()`, `new_lua_fn()`, `new_string()` (when not interned)
  - Need data from real fleet scripts to tune:
    - Initial threshold (currently 20 - probably too low?)
    - Growth factor (currently 2x)
  - **Blocked on:** Real fleet scripts running in fcomm2 to measure typical allocation patterns

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

- [x] **O(n²) upvalue closing** (`vm.rs` `State::close_upvalues()`)
  - `Vec::remove(0)` is O(n), called in a loop
  - **Fixed:** Reversed sort order (ascending) and pop from end in O(1)

### HIGH PRIORITY - Performance Optimizations

- [x] **Batch cost checking** (`frame.rs` `Frame::eval()`)
  - Accumulated cost locally, flushed every 64 ops
  - Reduces per-operation overhead by avoiding budget check on every costed op

- [x] **Replace GC linked list with arena** (`object.rs` `GcHeap` struct)
  - Linked list traversal was cache-hostile (each node is separate allocation)
  - **Fixed:** Chunked arena with `Vec<ArenaChunk>` where each chunk is `Box<[Slot; 256]>`
  - Slots are either `Occupied(WrappedObject)` or `Free { next_free: u32 }` (free list)
  - O(1) allocation via free list, linear sweep for cache-friendly GC

- [x] **Cache table `array_len`** (`table.rs` `Table::array_len()`)
  - Added `Cell<Option<usize>>` cache, invalidated on integer key changes
  - `array_insert`/`array_remove`/`set_array` update cache directly with known length

- [x] **Fixed-width 32-bit instruction encoding** (`instr.rs` `Instr` struct)
  - Previous instructions were ~16 bytes each (enum with isize variant)
  - 1000-instruction function = 16KB, may not fit L1 cache
  - **Fixed:** New 32-bit encoding `[opcode:8][A:8][B:8][C:8]` or `[opcode:8][A:8][sBx:16]` (4x smaller)
  - Jump offsets now use i16 (±32767 instructions, plenty for scripts)
  - Opcode-based dispatch in VM frame.rs using `match inst.opcode()`

### MEDIUM PRIORITY - Performance Optimizations

- [x] **Add `#[inline(always)]` to hot paths**
  - Added to: `consume_cost`, `instr_get_local`, `instr_set_local`, `pop_val`,
    `eval_float_float`, `eval_float_bool`

- [x] **Avoid cloning `Chunk` in closures**
  - Changed `Closure.chunk` and `Frame.chunk` to `Rc<Chunk>`
  - Closure copies now just clone the Rc, not the entire chunk

- [x] **Replace `Rc<RefCell<Upvalue>>` with arena** (`vm.rs` `State::find_or_create_upvalue()`)
  - Three allocations per upvalue + atomic refcounting on every clone
  - Lifetime is well-defined: closed when owning frame returns
  - **Fixed:** `UpvaluePool` stores upvalues in contiguous Vec, `UpvalueRef` is just a u32 index
  - Upvalues never freed until VM dropped (fine for short-lived game scripting VMs)

- [x] **Pre-size stack vector** (`vm.rs` `State::empty()`)
  - Stack pre-sized to 256, string_literals pre-sized to 64

- [x] **Small-table optimization** (`table.rs` `Table` struct)
  - HashMap overhead was high for 1-4 entry tables
  - **Fixed:** `TableStorage` enum with `Inline { entries: [(Val, Val); 4], len: u8 }` for small tables
  - Tables with ≤4 entries use inline array with linear scan
  - Automatically promotes to `IndexMap` when exceeding capacity or for shift operations

- [x] **String interning overhead** (`object.rs` `GcHeap::new_string()`)
  - Every allocation went through HashSet lookup + separate Box allocation per string
  - **Fixed:** `StringPool` with chunked arena storage + open-addressed hash table
  - FNV-1a hash with cached values in each entry (no rehashing on lookup)
  - Linear probing for cache-friendly collision resolution
  - Separate lookup/insert phases allow GC check only when allocating new strings

### LOW PRIORITY - Performance Optimizations

- [ ] **NaN-boxing for Val** (`lua_val.rs` `Val` enum) - DEFERRED (too invasive)
  - Current Val is ~16-24 bytes, every stack op copies this
  - Would reduce to 8 bytes by packing values into NaN bit patterns
  - **Analysis (Jan 2026):** Requires rewriting ~8-10 core files (lua_val.rs, frame.rs, eval.rs, metamethod.rs, table.rs, table_ops.rs, stack.rs). All pattern matching becomes bit extraction. RustFunc (function pointers) need special handling since they require 64 bits. High risk of subtle bugs in bit manipulation. GC marking must still correctly identify heap pointers.
  - **Recommendation:** Only pursue if profiling shows Val copying as a bottleneck

- [ ] **Superinstructions for common patterns** - DEFERRED (need usage data)
  - `GetLocal` + `GetField` (method dispatch)
  - `PushNum` + `Add` (constant arithmetic)
  - `Call(1, 1)` (single-arg single-return calls)
  - **Analysis (Jan 2026):** Test scripts aren't representative of real fleet scripts. Need actual game script corpus to identify which patterns are worth fusing. Premature optimization without data.

- [ ] **Pointer tagging for Val** - DEFERRED (needs NaN-boxing)
  - If allocations are 8-byte aligned, use low bits for type tags
  - **Analysis (Jan 2026):** Pointer tagging alone doesn't help much. The issue is f64 and RustFunc both need all 64 bits, so Val can't shrink to 8 bytes without NaN-boxing. Tagging just the pointer variants while keeping a 16-byte enum doesn't provide meaningful benefit. Only worth pursuing as part of full NaN-boxing effort.

- [x] **Fixed-size array for well-known globals**
  - `print`, `pairs`, `ipairs`, `type`, etc. accessed frequently via HashMap string lookup
  - **Fixed:** `Builtin` enum with 19 well-known names, `builtins: [Val; 19]` array in State
  - New `GetBuiltin`/`SetBuiltin` opcodes use direct array indexing (no hash lookup)
  - Compiler emits fast opcodes for well-known names, fallback to global for others

- [x] **Use `#[cfg(feature = "debug_vm")]` instead of `option_env!`** (`frame.rs` `Frame::eval()` debug print)
  - **Fixed:** Added `debug_parser`, `debug_vm`, `debug_gc` features to Cargo.toml

### API Improvements

- [x] **Replace magic 255 values with proper types** (`vm.rs` `State::call()`, `instr.rs` `Instr::Return`)
  - `num_args == 255`: vararg call base
  - `num_ret_expected == 255`: return all
  - `Return(255)`: return all
  - **Fixed:** Added `ArgCount` enum (Fixed/Dynamic) and `RetCount` enum (Fixed/All)
  - Public API `State::call()` now takes semantic types instead of raw u8 values

### Code Organization

- [x] **Split `vm.rs` (~1300 lines)**
  - Extracted metamethod handling to `vm/metamethod.rs`
  - Extracted stack operations to `vm/stack.rs`
  - Extracted table operations to `vm/table_ops.rs`
  - Extracted evaluation/call logic to `vm/eval.rs`
  - Core vm.rs now ~200 lines (struct, constructors, globals, cost budget)

- [x] **Improve test coverage**
  - Metamethod edge cases (`metatable_edge_cases.lua`)
  - Upvalue stress tests (`upvalue_stress.lua`)
  - ~~GC stress scenarios~~ (skipped - not needed for game scripting)
  - Error recovery paths (`error_cases.lua`)

### User Experience

- [ ] **Make VM informative for users**
  - Add source line numbers to Chunk for better error messages
  - Show function names in stack traces
  - Runtime cost warnings (e.g., "approaching budget limit")
  - Helpful suggestions in common error scenarios

- [ ] **Review error handling for non-programmers**
  - Audit all error messages for clarity (avoid jargon like "stack index", "upvalue")
  - Add "did you mean?" suggestions for typos in function/variable names
  - Explain common mistakes (e.g., "table has no field 'x' - did you forget to initialize it?")
  - Test error messages with non-programmer users

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
- Arithmetic metamethods (`__add`, `__sub`, `__mul`, `__div`, `__mod`, `__pow`, `__unm`)
- Comparison metamethods (`__eq`, `__lt`, `__le`)
- Concat metamethod (`__concat`)
- Long strings (`[[...]]`, `[=[...]=]`)

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
- [x] Multiple return values as function arguments (`add(vals())` passes all returns)
- [x] Loop variable capture fix - each iteration now has its own local binding via `CloseUpvalues`
- [x] `math.min`/`math.max` now accept varargs (was only comparing first 2 args)
- [x] Fix `ipairs` to continue on `false` values (only stop on `nil`)
- [x] Fix `%` modulo operator to use Lua's floored semantics (not C's truncated remainder)
- [x] String comparison operators (`<`, `>`, `<=`, `>=`) for lexicographic ordering
- [x] `rawequal(v1, v2)` - primitive equality without __eq metamethod
- [x] `rawlen(v)` - length without __len metamethod
- [x] Fix crash with empty patterns in string functions
- [x] String escape sequences (`\n`, `\t`, `\r`, `\\`, `\"`, `\'`, `\0`, `\a`, `\b`, `\f`, `\v`)
- [x] Multi-line comments (`--[[ ... ]]`)
- [x] Replace VM panics with proper `InternalError` for corrupt bytecode cases
- [x] Uppercase hex literals (`0XFF` in addition to `0xff`)
- [x] Empty table with separator (`{;}` and `{,}`)
