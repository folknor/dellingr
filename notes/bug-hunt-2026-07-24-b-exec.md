# Bug hunt 2026-07-24 - Corner B: Execution core

Auditor scope: `src/instr.rs`, `src/vm/eval.rs`, `src/vm/eval_control.rs`,
`src/vm/frame.rs`, `src/vm/stack.rs`, `src/vm/lua_val.rs`, `src/vm_aux.rs`,
`src/lib.rs` (plus supporting reads of `vm.rs`, `eval_index.rs`,
`eval_store.rs`, `object.rs`, `table.rs`, `metamethod.rs`, `compiler.rs`
for wiring evidence). Read-only audit; repros below are written, not run.

Findings are ordered most-severe first. Each says CONFIRMED (evidence is
structural and complete from reading) or PLAUSIBLE (needs a run to pin down).

---

## B1. `Val` equality/hash for `RustFn` compares payload addresses, not function pointers (CONFIRMED, correctness)

`src/vm/lua_val.rs:178-195` (PartialEq) and `:153-176` (Hash):

```rust
(RustFn(a), RustFn(b)) => {
    let x: *const RustFunc = a;   // a: &RustFunc  ->  pointer to the fn-ptr
    let y: *const RustFunc = b;   //                   STORAGE inside the Val
    x == y
}
```

`a`/`b` are `&RustFunc` (match ergonomics). Coercing `&RustFunc` to
`*const RustFunc` yields the address of the slot *holding* the fn pointer
(i.e. the payload inside each `Val`), not the function's address. Two
distinct `Val`s holding the same `RustFunc` always live at different
addresses, so equality is effectively **always false** for host functions.
`Hash` (`lua_val.rs:169-172`) has the identical mistake: it hashes the
payload address (`let f: *const RustFunc = func; f.hash(...)`), which is
also nondeterministic across runs (stack/heap addresses).

Reachability (all script-visible):

- `OP_EQUAL` in the dispatch loop (`src/vm/frame.rs:262-271`) uses
  `val1 == val2` directly: `print == print` evaluates **false**
  (reference Lua: true). Same for `local f = print; f == print`.
- `rawequal(print, print)` (`src/vm/stack.rs:215-220` -> `v1 == v2`) -> false.
- Host functions as table keys: `t[print] = 1; t[print]` returns nil, and
  repeated assignment appends duplicate entries (inline scan and IndexMap
  probe both fail the eq). Hashing the probe vs. the stored key also
  produces different hashes, so Map-storage lookups always miss.
- `string.format("%p", fn)`: `format_pointer_id`
  (`src/vm/table_ops.rs:432-445`) probes `format_pointer_ids` with
  `*candidate == val`; for a `RustFn` this never matches, so **every**
  `%p` on the same host function mints a fresh id, defeating the
  deterministic-identity feature for functions (tables are unaffected).

The codebase already knows the right tool: `std::ptr::fn_addr_eq` is used
in `src/vm/eval_control.rs:138-143`. Fix: compare/hash `*a as usize`
(or `fn_addr_eq`) in both impls. Note TODO.md's "Stable RustFunc identity"
entry describes lines 105/169 as hashing "by function-pointer address" -
that description is wrong for the `&self` impls; the bug is stronger than
what TODO tracks.

Related inconsistency, same file: `fmt::Debug`/`fmt::Display` for
`Val::RustFn` (`lua_val.rs:130,145`) format `func: &RustFunc` with `:p`,
which prints the **reference's** address (payload slot), while
`to_string_with_heap` (`:105`, takes `self` by value) prints the actual
function address. So `tostring(print)` and a debug-printed error context
show different "addresses" for the same value.

Repro:

```lua
print(print == print)          -- dellingr: false, Lua: true
local t = {}
t[print] = 1
print(t[print])                -- dellingr: nil, Lua: 1
print(string.format("%p", print) == string.format("%p", print))
                               -- dellingr: false (two fresh ids)
```

## B2. Frame varargs are not GC roots: script-reachable use-after-free panic (CONFIRMED, GC integrity / panic)

`eval_closure_inner` (`src/vm/eval.rs:309-314`) drains vararg arguments
off the VM stack into a plain `Vec<Val>`:

```rust
let varargs = ... self.stack.drain(vararg_start..).collect();
```

That Vec is moved into the `Frame` (`src/vm/frame.rs:29`), which is a
**Rust local** in `eval_closure_inner` - it is not part of `State`.
`mark_gc_roots` (`src/vm.rs:69-89`), the declared single source of truth,
marks stack, globals, builtins, string_literals, active_call_roots,
upvalue pool (transitively), and the anchor registry - **frame varargs are
in none of these sets**. Any allocation inside the frame (table
constructor, string concat, closure creation, or the per-call literal
interning in `initialize_frame` itself, `eval.rs:424-443`) can trigger
`gc_collect` while a heap value is reachable *only* through
`frame.varargs`. It gets swept; the later `OP_VARARG` (`frame.rs:229-247`)
pushes the stale `ObjectPtr`/`StringPtr`, and the next heap access panics
with "Invalid ObjectPtr: object was freed (use-after-free detected)"
(`src/vm/object.rs:221-234`). A script kills the host process (panic),
violating "errors kill the callback, not the host".

The window covers both `initialize_frame`'s literal-interning loop
(varargs Vec held while `alloc_string` may collect) and the entire
`frame.eval` duration. Tests likely miss it because benches pass numeric
varargs (`Val::Num` is not heap-managed).

Repro (any script where a table/string reaches a frame only via `...`,
with enough allocation to trigger GC before the `...` is re-expanded):

```lua
local function f(...)
  local junk
  for i = 1, 200 do junk = {i} end   -- drive collections (threshold starts at 20)
  return ...
end
print(f({ "boom" }))   -- expected: table printout; actual: Rust panic
```

Fix directions: (a) add active frames' varargs to the root set (requires
frames to be visible to State - see optimization O1, which fixes this
structurally), (b) keep varargs on the VM stack below the frame like
reference Lua, or (c) stash the varargs Vec in a State-owned side stack
(`Vec<Vec<Val>>` pushed/popped around each frame) that `mark_gc_roots`
walks. (c) is the minimal targeted patch.

Note the same "unrooted detached copy" pattern exists outside my corner in
`table_sort` (`src/vm/table_ops.rs:283-353`): `arr` values survive only in
the table while comparators run, but a comparator that deletes keys from
the table (`t[k] = nil`) leaves `arr` holding unrooted values that get
written back stale by `set_array`. Flagging for the table/stdlib corner.

## B3. NaN comparisons: `<=` and `>=` evaluate true (CONFIRMED, correctness vs 5.2/5.4)

`eval_compare` (`src/vm/eval_store.rs:484-509`) maps
`partial_cmp -> None` (NaN involved) to `Ordering::Equal`, and the dispatch
implements `<=` as negated `>` and `>=` as negated `<`
(`src/vm/frame.rs:277-278`). For NaN operands the comparison result is
`Equal`, which is not `Greater`/`Less`, so the negated forms return
**true**. Reference Lua: every ordered comparison involving NaN is false.

```lua
print((0/0) <= 1)   -- dellingr: true, Lua: false
print((0/0) >= 1)   -- dellingr: true, Lua: false
print(1 <= 0/0)     -- dellingr: true, Lua: false
print((0/0) < 1)    -- both: false (only <=/>= are wrong)
```

Fix: on `partial_cmp() == None`, push `false` unconditionally (before the
`negate` step), or compute `<=` as a first-class comparison instead of
`!(>)`.

## B4. `next` with a NaN key panics on Map-storage tables (CONFIRMED, script-reachable panic)

`Val`'s `Hash` hard-asserts `!n.is_nan()` (`src/vm/lua_val.rs:162`).
`Table::insert`/`get`/`get_with_index` guard NaN before hashing, but
`Table::next` (`src/vm/table.rs:512-548`) does not: the Map arm calls
`map.get_index_of(key)`, which hashes the key. A NaN control value reaches
it two ways:

- stdlib: `next(t, 0/0)` -> `base_next` -> `State::table_next`
  (`src/vm/table_ops.rs:155-176`) -> `t.next(&NaN)`.
- dispatch fast path: `for k in next, t, 0/0 do end` ->
  `instr_tfor_call_next` (`src/vm/eval_control.rs:186-207`).

Inline storage (<= 4 entries) survives (linear `PartialEq` scan, NaN
compares unequal, returns nil - itself a soft divergence, see B9). Map
storage (> 4 entries) hits the assert: **process panic** from pure script.
Reference Lua raises "invalid key to 'next'".

```lua
local t = {}
for i = 1, 10 do t[i] = i end   -- force Map storage
for k in next, t, 0/0 do end    -- dellingr: panic; Lua: error
```

Fix: reject NaN (and consider missing keys, B9) at the top of
`Table::next` with a proper Lua error.

## B5. Floored-modulo formula produces NaN for infinite divisor (CONFIRMED, correctness)

`OP_MOD` (`src/vm/frame.rs:328-333`) computes `a - (a/b).floor() * b`.
With `b = inf`: `a/b = 0`, `0 * inf = NaN`, result NaN. Reference Lua
(5.2 and 5.4 `luai_nummod`) is fmod-based: `1 % math.huge == 1.0`,
`-1 % math.huge == inf`. The formula is also less exact than `fmod` for
large finite operands (rounding in `a/b` can flip the floor).

```lua
print(1 % (1/0))    -- dellingr: nan,  Lua: 1.0
print(-1 % (1/0))   -- dellingr: nan,  Lua: inf
```

Fix: implement as reference does: `m = a.rem(b) (fmod); if m != 0 && (m < 0) != (b < 0) { m += b }`.

## B6. `instr_tfor_call_rust_fn` truncates the result count with `as u8` (CONFIRMED code defect; PLAUSIBLE impact - host-API only)

`src/vm/eval_control.rs:266`: `let num_ret_actual = self.get_top() as u8;`.
If a host-provided iterator RustFunc leaves more than 255 values on its
frame, the cast wraps (e.g. 256 -> 0), the `Greater` arm then pushes
spurious nils, and the subsequent `results_start`/`truncate` bookkeeping
leaves hundreds of stray values on the stack inside the loop frame -
silent stack corruption instead of the "too many results" error. The
sibling path in `State::call` (`src/vm/eval.rs:102-117`) does this
correctly in `usize`. Not reachable from stdlib iterators; requires a
host RustFunc used as a generic-for iterator. Fix: mirror the `usize`
comparison from `State::call`.

## B7. `tostring(fn)` leaks ASLR-dependent addresses: cross-run output nondeterminism (CONFIRMED, determinism)

`to_string_with_heap` (`src/vm/lua_val.rs:105`) renders host functions as
`<function: 0x...>` using the real function pointer. Under PIE/ASLR that
address changes between two runs of the same binary, so
`print(tostring(print))` produces different output on identical runs -
in tension with the determinism contract ("identical runs on identical
hosts produce identical results"). Tables already solved this: `ObjectPtr`
Display prints the slotmap key (deterministic), and `%p` mints
deterministic ids. TODO.md tracks pointer-identity only for
*serialization*; the script-observable `tostring` path is not covered.
Fix: route function rendering through `format_pointer_id`-style
deterministic ids (also fixes the B1-related Debug/Display inconsistency).

## B8. Concat is cost-free: uncharged unbounded work and memory growth (CONFIRMED behavior; flag as design review, cost-model integrity)

`OP_CONCAT` (`src/vm/frame.rs:304-307`) charges nothing, and
`concat_helper` (`src/vm/eval.rs:228-263`) does O(total bytes) work and
allocation. `s = s .. s` in a free `while true` loop doubles memory every
iteration with **zero** cost charged - ~40 iterations exceeds any realistic
host memory. The in-code comment declares string ops free, and README's
Budget section concedes structural freebies, but exponential *allocation*
is a different hazard class from a free busy-loop: it OOMs the host rather
than idling a tick budget. Recommend charging concat proportional to
output length (like SET_LIST charges per element), or a max-string-length
cap. Decision belongs to the cost-model owner; recording it here because
the charge site is in dispatch.

## B9. `next(t, k)` with a non-member key silently ends iteration (CONFIRMED, minor divergence)

`Table::next` returns `(nil, nil)` when the key isn't found (both storage
arms, `src/vm/table.rs:512-548`); reference Lua raises
"invalid key to 'next'". Affects `next()` directly and the
`instr_tfor_call_next` fast path (a mutation-during-traversal bug in user
script terminates silently instead of erroring). Low priority; same
function as the B4 fix.

## B10. Per-function cap of 255 `SET_FIELD` sites is a hard compile error (CONFIRMED, limitation)

`assign_cache_slots` (`src/compiler.rs:259-269`) errors with
`TooManyFieldAssignments` when one function body contains more than 255
`t.x = v` statements (SET_FIELD packs its cache index into the 8-bit C
slot, `src/instr.rs:509-511`). Reference Lua compiles arbitrarily many.
Generated/data-heavy scripts can plausibly hit this. Cheap fix: on
overflow, stop assigning cache slots (sentinel index = uncached slow
path) instead of failing the compile; `instr_set_field` already tolerates
`cache = None` via `caches.set_field_lookup.get(idx)`... note today every
index < 255 resolves to a slot, so the sentinel needs to be an
out-of-range value (e.g. keep 255 reserved as "no cache").

## B11. Stack-trace line staleness for non-OP_CALL invocations (CONFIRMED, minor diagnostics)

Only `OP_CALL` refreshes `call_info.ip` (`src/vm/frame.rs:206-213`).
Calls made by `OP_TFOR_CALL`, `__index`/`__newindex`/`__len`/`__tostring`
metamethod dispatch, and `table.sort` comparators leave the caller's
`CallInfo.ip` pointing at the previous `OP_CALL`, so error tracebacks
report the wrong caller line for errors raised inside iterators and
metamethods. Fix: refresh `ip` in those dispatch sites too (or derive it
from the live `Frame` at trace-build time).

## B12. Small robustness notes (no action strictly required)

- `Frame::jump` (`src/vm/frame.rs:83-99`) accepts `ip == code.len()`;
  the next `get_instr` would panic on the out-of-bounds fetch. Unreachable
  with compiler-emitted bytecode (every chunk ends with OP_RETURN), but
  the bound should be `<` for defense in depth.
- `State::set_top` with a large positive index (`src/vm/stack.rs:25-46`)
  pushes nils without a `MAX_STACK_SIZE` check - a host can blow past the
  stack cap that `check_stack_space` enforces on the call path. Host
  misuse only.
- `pop`/`set_top` use `assert!` (panic) for host misuse where the rest of
  the host API returns `Result`. Inconsistent contract; consider errors.
- Numeric `for` with step 0 skips the loop (`eval_control.rs:316-326`).
  Matches 5.2 for ascending ranges, silently diverges from 5.2's infinite
  loop for descending ranges and from 5.4's "'for' step is zero" error.
  Looks like a deliberate sandbox choice - worth one line in README's
  divergence notes if so.
- Arithmetic does not coerce numeric strings (`"10" + 1` is a type error;
  reference Lua yields 11; `eval_float_float`/`pop_num`,
  `eval_store.rs:511-530`). Concat *does* coerce numbers to strings. If
  the strictness is deliberate (it reads that way), it belongs on the
  README "Won't implement" list; today it's an undocumented divergence in
  an implemented feature area.

## Verified-clean items worth recording

- Budget boundary: the `add_cost!` batching in `frame.rs:147-159` plus
  `consume_cost` (`vm.rs:329-339`) enforces exactly "the op that crosses
  the boundary completes; the next costed op fails", including flushes
  before OP_CALL/OP_TFOR_CALL/OP_RETURN and before all metamethod
  invocations (`metamethod.rs`, `instr_length`). One soft edge: up to 63
  accumulated cost is dropped (never added to `cost_used`) when a frame
  errors mid-batch - only reporting accuracy, not budget enforcement.
- Dynamic SET_LIST: sentinel is count==0 (not 255); `analyze_cost`'s
  `OP_SET_LIST => n = inst.a()` therefore adds 0 for dynamic
  constructors, consistent with the runtime minimum; runtime charges the
  actual element count before inserting. Base validation (frame range,
  is-table) and the error-path watermark truncation of
  `vararg_call_bases`/`table_constructor_bases` (`eval.rs:274-292`) hold.
- 255 ceilings: Dynamic arg count, RetCount::All result count, `__call`
  argument prepend (checked_add), and `unpack`/`select` all error cleanly
  at the ceiling. The `__call`-chain recursion in `State::call` is bounded
  by the same ceiling.
- Cache-slot aliasing: `finalize`/`assign_cache_slots` rewrites *every*
  GET_GLOBAL/GET_FIELD/SET_FIELD with a real slot index, so the plain
  constructors' implicit index 0 never reaches the runtime.
- `with_restricted_env` swaps are honored by all three IC families via
  `globals_version` snapshots, and restore under panic via catch_unwind.
- `mark_gc_roots` coverage is otherwise sound for the execution core:
  active closures via `active_call_roots`, per-frame literals via
  `string_literals`, metamethod key/val protection pushes in
  `metamethod.rs` are all present. B2 (varargs) is the one hole found.

---

# Optimization opportunities (execution core)

Ranked by expected real-throughput impact. Determinism, per-opcode cost
accounting, and the Won't-implement list are preserved by all of these.

## O1. Full coherent rewrite: flatten the interpreter into an explicit frame stack

Today every Lua-to-Lua call recurses through Rust
(`State::call` -> `eval_closure` -> `eval_closure_inner` -> `Frame::eval`),
with per-call costs that are all consequences of the frame being a Rust
stack local:

- `Closure` clone per call (`GcHeap::as_lua_function`,
  `object.rs:237-241`): clones the upvalues `Vec<UpvalueRef>` (heap
  alloc per call for any closure with upvalues) + 2 Arc refcount bumps.
- Duplicate frame bookkeeping: `CallInfo` push (another `Arc<Bytecode>`
  clone, `eval.rs:304-307`) parallel to the `Frame` itself.
- `stack.remove(idx)` to extract the callee (O(args) memmove,
  `eval.rs:68`).
- varargs `drain(..).collect()` Vec per vararg call (`eval.rs:310-314`).
- return values `drain(..).collect()` into a fresh `Vec` then `extend`
  back (`eval.rs:404-416`) - an allocation per returning call that could
  be a single in-place `stack.drain(bottom..ret_start)` memmove.
- per-call string-literal interning (see O2).

A single dispatch loop over a `State`-owned `Vec<FrameState>` (bytecode
Arc, ip, base, vararg span, cache ptr) eliminates every item above,
merges `Frame` with `CallInfo` (stack traces read the real frames), and -
critically - makes frames visible to `mark_gc_roots`, which **structurally
fixes B2** instead of patching it. Calls become "push frame record,
continue loop"; returns become "memmove rets down, pop frame record".
MAX_CALL_DEPTH becomes a plain length check, and the AGENTS.md concern
about Rust-stack bloat per recursion level disappears. This is the
highest-leverage rewrite available in the execution core; call-heavy
benches (`calls/*`, `benchmark`) are the ones sitting at ~4x lua5.5.
Pre-1.0, internal-only: `State::call`'s public signature can stay.

## O2. Intern chunk string literals once per (State, Bytecode), not per call

`initialize_frame` (`eval.rs:424-443`) re-interns *all* of a chunk's
string literals into `state.string_literals` on **every call**, and
truncates them on return - k intern-pool probes + pushes + per-literal
GC-threshold checks per call, where k counts every field name and string
constant in the function. This is pure per-call overhead for steady-state
code. Replace with a per-State cache keyed on `Bytecode` identity holding
the interned `Vec<Val>`, marked as a GC root (or pin literal strings for
loaded programs). This is the same `IndexMap<*const Bytecode, ...>`
infrastructure OPTIMIZATIONS.md already sketches for sharing
`RuntimeCaches` across closures - one mechanism can serve both, so I'd
fold them into a single work item rather than re-proposing that entry.
`get_string_constant` then becomes a direct slice index and the
`string_literal_start` frame plumbing disappears.

## O3. SET_GLOBAL inline cache and allocation-free global writes

`instr_set_global` (`eval_store.rs:361-379`) does UTF-8 validation +
`String` allocation + IndexMap hash lookup on every global assignment;
`set_global_value_owned` re-checks `Builtin::from_name`. GET_GLOBAL
already has an IC (`globals_version` + index). Mirror it for writes:
cache the entry index per SET_GLOBAL site (slot in `RuntimeCaches`),
validated by `globals_version`; on hit, write through
`globals.get_index_mut` with zero allocation. The builtin check can be
resolved at compile time (the parser already emits SET_BUILTIN for known
names, so the runtime `Builtin::from_name` re-check in
`set_global_value_owned` is only needed on the cold path).

## O4. Gate the table-library fallback off the field-read miss path

Every plain-table field read that misses (no metatable, key absent) - the
extremely common `if t.optional_field then` pattern - takes
`push_table_library_field` (`eval_index.rs:351-363`): a global lookup of
`table` plus a full `get_table_with_key` against the library table,
before returning nil. The fallback feature (`t:insert(...)` sugar) only
ever resolves the handful of table-lib method names. Cheap gate: check
the key against a compile-time set of table-lib names (they are string
literals; a per-chunk bitmask computed at literal-pool build time, or an
interned-ptr comparison against the few lib method strings) and skip the
fallback entirely for all other keys. Alternatively only emit the
fallback-capable GET_FIELD variant in method-call position, where the
sugar is actually used.

## O5. Local cleanups in the call path (worthwhile even if O1 lands later)

- Return values: replace drain-to-Vec + extend with
  `self.stack.drain(self.stack_bottom..ret_start);` (values above move
  down in one memmove; do it after `close_upvalues`). Removes one heap
  allocation per returning call today.
- `Closure.upvalues` as `Arc<[UpvalueRef]>` (or a SmallVec inline <= 2):
  makes the per-call `Closure` clone allocation-free.
- `concat_helper`: numbers are formatted with `format!` (a `String`
  allocation per number operand); write into the existing `buffer` via
  `std::fmt::Write`/itoa-style instead. Also the 32-byte estimate is fine
  but the two-pass loop reads the heap twice per string; single pass with
  a reserve is simpler and faster.
- `State::call` fixed-arg path: avoid `stack.remove(idx)` by treating the
  callee slot as frame slot -1 (adjust `stack_bottom` instead); pairs
  naturally with O1.

## O6. Dispatch micro-items (verify with asm/bench before committing)

- Opcode space is sparse (0-25, 30-54, 60-63, 70-72). A dense renumbering
  (or `#[repr(u8)]` enum with unsafe-free `match` on a validated dense
  range) helps LLVM emit a single dense jump table without range holes.
- `get_instr` bounds-checks every fetch; with a one-time validation that
  all jump targets are in-bounds at load/finalize time (they already are,
  from `assign_cache_slots`' walk), the fetch could use a pointer/len pair
  cursor. Only worth it if profiles show it; keep panics over UB.

Explicitly not re-proposed (already tracked in OPTIMIZATIONS.md): shared
RuntimeCaches per Bytecode (folded into O2), shape/key-position field ICs,
array-part storage, field-update fusion, per-Engine stdlib install,
cache-Vec pooling.
