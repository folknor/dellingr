# Hygiene hunt: vm scope

Scope: `src/vm.rs`, `src/vm/` (excluding save_state), `src/vm_aux.rs`. Read-only
pass at 576c0f5. Findings are unranked. Each says what enforcement could hold
the fixed version.

## 1. Bugs found along the way (verify first)

### 1a. `__call` chain has no depth guard - likely native stack overflow
`State::call` (vm/eval.rs, the `__call` branch) re-enters `self.call(..)`
recursively when the callee is a table whose metatable has `__call`. Neither
`call_depth` (only bumped in `eval_closure`) nor `metamethod_depth` (only
`__index`/`__newindex`, vm/metamethod.rs) is touched. `setmetatable(t,
{__call = t}); t()` recurses in Rust, growing the value stack by one slot per
level; the 1,000,000-slot cap is far beyond what the native stack survives, so
this should abort the process instead of returning an error. The README's
"catchable rather than an abort" promise is broken on this path. Not run - needs
a test. Fix: route `__call` through the same depth counter as the other
metamethods (or loop instead of recursing). Enforce with a regression test.

### 1b. gmatch errors report an absolute stack index as the argument number
`memoize_gmatch_pattern` and `gmatch_subject_matcher_and_cost_meter` (vm.rs)
build `ArgError { arg_number: idx as isize + 1 }` where `idx` is the result of
`convert_idx`, i.e. an absolute stack index. Any non-empty stack below the
frame makes the reported argument number wrong. `push_bytes_from_stack_range`
and `to_bytes` (vm/stack.rs) correctly use the caller's relative index. The
four copies of this "expected string" ArgError literal are the root cause (see
3c).

### 1c. Concat and tostring render numbers by different rules
`concat_helper` (vm/eval.rs) renders `fract()==0 && abs<1e15` as `num as i64`,
else `{num}`; `Val::to_bytes_with_heap` (vm/lua_val.rs) uses `n.to_string()`.
They already diverge: `-0.0 .. ""` yields `"0"`, `tostring(-0.0)` yields `"-0"`.
One number-to-string function should exist (and stdlib `tostring` /
`string.format` should read it too - other hunters' scope). Enforce with a
property test that `x .. ""` equals `tostring(x)` for all finite/special f64s.

### 1d. `cost_remaining()` is incoherent when no budget is configured
`State::consume_cost` always decrements `cost_remaining`, but
`cost_meter()` / the gmatch meter use `CostMeter::count_only` when
`cost_budget_configured` is false, which bumps `cost_used` only. So with no
budget, `cost_remaining()` drifts from `i64::MAX - cost_used`. Two writers of
one invariant. See 5a.

## 2. One value, one owner

- **GC initial/minimum threshold `20`**: `State::GC_INITIAL_THRESHOLD = 20`
  (vm.rs) and the literal `.max(20)` in `GcHeap::collect` (vm/object.rs), plus
  the growth factor `2` inline there. Already two owners; tests (vm/tests.rs)
  restate `>= 20` and `gc_set_threshold(20)`. Move the policy (initial, floor,
  growth) into `GcHeap` as named consts, or a `GcPolicy` struct. Enforceable by
  a text rule only weakly; structurally by making the heap the sole owner.
- **`usize::MAX` = auto-GC disabled sentinel**: spelled in `gc_disable_auto`
  (vm.rs), in `GcHeap::collect`, and in doc comments. Replace with
  `Option<usize>` or an enum threshold so the sentinel is unrepresentable.
- **"255" argument/result limit**: `"too many arguments (limit 255)"` twice and
  `"too many results (limit 255)"` once in vm/eval.rs, again in
  lua_std/basic.rs; ad hoc `RuntimeError` strings, while the compiler side has
  typed variants (`TooManyExpressions` etc. in error.rs). The 255 itself is the
  `ArgCount`/`RetCount` u8 width (instr.rs). Add `ErrorKind::TooManyArguments
  / TooManyResults { limit }` with the limit derived from `u8::MAX`. Note also
  the `__call` path computes `combined = actual + 1` which can reach 255,
  the value `ArgCount`'s byte encoding uses as the Dynamic sentinel; harmless
  as an enum today but worth a test.
- **MAX_CALL_DEPTH = 1000, MAX_STACK_SIZE = 1_000_000, MAX_METAMETHOD_DEPTH =
  200** are private consts in two files (vm.rs, vm/metamethod.rs), restated as
  numbers in README.md, AGENTS.md, error.rs doc comments ("runs 1000 frames
  deep"), and the eval.rs comment ("MAX_CALL_DEPTH = 1000"). Only
  `MAX_STRING_BYTES` is public. Make them public consts beside
  `MAX_STRING_BYTES` (one `limits` module) and reword prose to cite the name,
  not the value. The error variants `CallDepthExceeded`, `StackOverflow`,
  `MetamethodDepthExceeded` carry no `limit` field, unlike
  `StringSizeExceeded { size, limit }` - inconsistent; give them all `limit`.
  A doctest asserting README numbers equals the consts would hold it.
- **String size check duplicated**: `check_string_size` (vm.rs) vs an inline
  `len > MAX_STRING_BYTES` building the same error in save_state.rs.
  `resolve_bytecode_runtime_no_gc` skips the check entirely (relies on
  load-time validation elsewhere).
- **Metamethod name strings**: `"__index"` (metamethod.rs, eval_index.rs),
  `"__newindex"`, `"__call"`, `"__len"`, `"__tostring"` each interned by
  `alloc_string` at the use site on every dispatch, plus `"__index"` /
  `"__newindex"` / `"__metatable"` in lua_std/basic.rs. Pre-intern a
  `MetaName` enum once per State (rooted), like `Builtin`. Removes repeat
  interning, removes four `.expect("fixed metamethod name is far below
  MAX_STRING_BYTES")` copies, and removes the GC-rooting dance (see 5c).
- **`"<function>"`**: `RUST_FN_DISPLAY` const exists in vm/lua_val.rs, but
  `to_bytes_with_heap` in the same file spells `b"<function>"` literally.
- **BudgetExceeded construction**: `consume_cost` and `budget_exceeded_error`
  (vm.rs) both build it. `consume_cost` should call the helper.
- **CostMeter selection**: `cost_meter()` and the destructured copy in
  `gmatch_subject_matcher_and_cost_meter` (vm.rs) each restate "configured ->
  finite_budget else count_only". Extract a free fn over the fields.
- **Capacity hints**: `stack` 256, `call_stack` 64, `transient_roots` 64,
  `reserve_stdlib_capacity` (`COUNT + 4`, `8, 96`), table `INLINE_CAPACITY 4`.
  Each defined once but at first use; nothing lists the tunables. Low stakes;
  a `tuning` section with named consts would answer "what are the knobs".
- **`COST_CHECK_INTERVAL = 64`** (vm/frame.rs) is a determinism-relevant knob
  only if overshoot semantics differ; the add_cost macro pre-checks
  `cost_remaining` so it should not. Document that it is not observable, or
  test that cost_used is invariant to it.

## 3. Values nobody can find, change, or trust

- **Limits have no injection point.** All depth/stack/GC limits are compile-time
  consts; `call_depth_exceeded_error` must really recurse 1000 frames on a
  dedicated big-stack thread (tests/error_handling.rs), and the stack-cap
  tests fill a million slots. A `Limits` struct on `State` (defaulted, settable
  by the embedder, fixed at construction for determinism) would let tests use
  small caps and let the game host tune per-script. Save/restore would then
  need to record it.
- **GMATCH cache bounds** (`GMATCH_PATTERN_CACHE_ENTRIES = 256`,
  `_BYTES = MAX_STRING_BYTES`) sit at the top of vm.rs, a stdlib concern living
  in the VM file. The bytes bound being "one maximal string" is an arbitrary
  coupling.
- **3c: the "expected string at index" ArgError** is hand-built at four sites
  (vm.rs x2, stack.rs x2). One `fn expect_string_at(idx)` helper fixes 1b.

## 4. One channel, one implementation

- `debug_vm` prints every instruction with `println!` (vm/frame.rs) and
  `debug_gc` prints with `println!` (vm/object.rs), bypassing `HostCallbacks`
  and interleaving with script `print` output on stdout. Should be stderr at
  least, or a `HostCallbacks::on_debug` hook. Enforceable by clippy
  `print_stdout`/`print_stderr` deny in lib.rs (currently not in the list),
  with the feature-gated sites allowed explicitly.
- `DefaultCallbacks::on_print` (host.rs) is the library's one sanctioned
  stdout write, and `on_error` default is presumably silent - `State::new()` in
  a library therefore writes to the host's stdout by default. Arguably the CLI
  should own that; the library default could be a no-op/collector.
- GC collections, budget exhaustion and depth-limit hits produce no
  host-visible event other than the error itself; no GC callback exists for a
  host that wants to observe collection pauses.

## 5. Errors

- **5a. Three implementations of the budget rule**: `State::consume_cost`,
  `CostMeter::consume`/`consume_units` (src/cost_meter.rs), and the `add_cost!`
  macro in `Frame::eval` (pre-checks `cost_remaining()` then calls
  `consume_cost`). They agree today except 1d. Make `CostMeter` the sole
  implementation and have `consume_cost` construct one.
- **Mislabelled error kinds**:
  - `InvalidStackIndex` renders as `"internal error: invalid stack index"`
    (error.rs) but is the documented result of host misuse of public
    `set_top`/`pop`/`convert_idx`.
  - `push_named_rust_fn` (stack.rs) and table_ops.rs map
    `SaveError::DuplicateFunctionRegistration` (an embedder bug) to
    `InternalError` (documented as "VM bug"), stringifying it and losing the id
    as a field.
  - gmatch maps `PatternError` to `RuntimeError(String)`, shedding structure.
- **Two spellings of "error with no location"**: `self.error(kind)` and
  `Error::without_location(kind)` are used interchangeably in vm/
  (`State::error` ignores `self`). Pick one; `State::error` could be removed
  from the public API or made to actually locate.
- **Aborts on embedder-controlled input** (public API):
  - `check_any`, `check_type`, `is_none_or_nil` `assert!(arg_number != 0)`
    (vm_aux.rs).
  - `get_table` `assert!(idx != self.stack.len() - 1)` (vm/table_ops.rs): a
    host passing the key's own index panics instead of getting
    `InvalidStackIndex`.
  - `Val`'s `Hash` impl hard-asserts non-NaN (lua_val.rs); safe only while
    every insertion path pre-rejects NaN.
  - `replace` / `pop_val` `expect` on empty stack - fine after `convert_idx`,
    but `insert`/`remove`/`replace` accept indices that `convert_idx`
    accepts and are otherwise unvalidated.
- **Frame-exit error cleanup copied**: `eval_closure_frame` (eval.rs) has two
  verbatim copies of locate + trace + `host_error` + close_upvalues + truncate
  + restore bottom + pop call_stack, a third partial copy on the
  check_stack_space failure, and `call()`'s RustFn path restores
  `stack_bottom` separately. A drop guard or single `unwind_frame(err)` would
  make "restore on all exit paths" (the long comment in `call`) structural.
  Note `self.pop(..)?` in the `Ordering::Greater` arm returns without restoring
  `stack_bottom`/`call_stack` (unreachable in practice, but it is exactly the
  bug class the comment warns about).
- **`on_error` once-only rule** is maintained by two writers: `call()` fires
  for depth 0 with empty trace, `eval_closure_frame` fires when it attaches a
  trace. The "empty stack_trace means not yet reported" encoding is implicit.
  A `reported: bool` on `Error` would be structural.

## 6. Policy invented per call site

- **GC trigger**: `if self.heap.is_full() { self.gc_collect() }` is copied in
  `alloc_string` (vm.rs), `alloc_table`, `alloc_table_with_capacity`
  (table_ops.rs), `push_closure` (eval.rs). One `maybe_collect()` would own it;
  the `GcHeap` allocators could require a token proving the check ran.
- **Rooting a temporary across allocation** has at least four idioms: push on
  the value stack and pop (metamethod.rs `__index`, `__newindex`),
  `with_rooted_value` (eval.rs `__call`), manual `transient_roots` push/pop
  (eval.rs Lua-call path, resolve_bytecode_runtime), and manual
  watermark+catch_unwind (eval_closure_inner varargs), plus `bytes_with_
  tostring_meta` which interns `"__tostring"` without rooting `val` at all
  (safe only because `val` is still on the stack). Pre-interned metamethod
  names (section 2) delete most of these sites.
- **Panic safety via catch_unwind** is applied in `with_restricted_env`,
  `with_rooted_values`, and vararg frames, but explicitly not on the main Lua
  call path (eval.rs comment: the State must be discarded after an unwind).
  So the partial guarantees buy nothing: a panic anywhere under a Lua call
  already leaves the State inconsistent. Either poison the State on unwind
  (a `poisoned` flag checked on entry) and drop the per-site catch_unwind, or
  make it uniform. Today it is half-policy.
- **Depth guards**: call depth (eval.rs) and metamethod depth (metamethod.rs,
  two copies of the inc/check/dec wrapper) each hand-roll the counter; `__call`
  has none (1a). A single RAII depth guard type would cover all three.

## 7. Ambient dependencies

- `anchor::NEXT_STATE_ID` is a process-global `AtomicU64`. Not a determinism
  leak today (only Anchor identity and Debug), but it is the one piece of
  cross-State global state; documented in anchor.rs.
- RNG is properly owned (`state.rng`, seed 0 default). Clean.
- `format_pointer_ids` ids are per-State deterministic. Clean.

## 8. Shared state and locking

- `with_restricted_env` swaps `globals`/`builtins` and bumps
  `globals_version`, but does not refresh `table_library_fallback`. It is safe
  only because the fallback cache compares the table ObjectPtr at use; the
  safety argument lives in the cache's doc comment, not in the swap.
- `pending_bytecode_caches` exists to bridge a GC that may run between
  `resolve_bytecode_runtime` and `alloc_lua_fn` in `push_closure`; correctness
  rests on the push/pop ordering in that one function.

## 9. Unbounded growth

- `format_pointer_ids` (Vec, linear probe) is only pruned in `gc_collect`; with
  `gc_disable_auto` it grows without bound and each `%p`/tostring of a table
  is O(n). Use an `IndexMap<Val,u64>` and/or prune independently.
- `open_upvalues` `find_or_create_upvalue` is a linear scan plus sorted
  `insert` - O(n) per capture; bounded by the stack cap but quadratic.
- `Registry` (anchors) is embedder-controlled and unbounded; acceptable, but
  `heap_size()` does not count it, so a host has no visibility.
- `bytecode_caches` entries are swept, but swept entries keep literals alive
  one extra GC (documented).

## 10. Not load-bearing / stale

- Comments citing audit ids `(L5)`, `(L6)`, `(L8)`, `(L10)`, `(L11)`, `(L17)`,
  `(L18)` (eval.rs, vm.rs, table_ops.rs, also lexer.rs, math.rs, table.rs,
  save_state.rs) reference a findings list that is not in the tree - durable
  code citing a transient note, which AGENTS.md forbids. Drop the ids.
- `State.globals` doc: "May be changed to an actual Table in the future" -
  speculative.
- `vm.rs`: "State marking is done through mark_gc_roots() which has direct
  heap access" - orphan comment.
- `Frame::eval` doc says string operations and length are free; README's cost
  model and the gmatch/pattern charging (`consume_cost(pattern_len)`) say
  otherwise. Stale, or at best incomplete.
- `load_string_named` doc mentions `"[fleet:123]"`, a game-specific
  leftover; harmless.
- `State::error` takes `&self` and ignores it; `concat` (public) rejects
  `n < 2` with an ArgError naming `"concat"` while its doc says n=0 yields the
  empty string and n=1 is a no-op - the doc contradicts the code.
- `bytes_coerce` is a pure alias of `bytes_with_default_string_coercion`.

## 11. Host assumptions

- No filesystem, env, clock or path access in scope. The only host
  assumption found is native stack depth: `MAX_CALL_DEPTH = 1000` is safe only
  on a sufficiently large native stack (tests spawn a big-stack thread; the
  error.rs and eval.rs comments describe frame-size budgeting to keep debug
  builds under it). An embedder calling into the VM from a small-stack thread
  (e.g. a 2 MiB worker, or musl's 128 KiB default) gets an abort, not
  `CallDepthExceeded`. Nothing detects or documents the required stack size
  for embedders; consider documenting a minimum and/or making the depth
  limit injectable (section 3). 1a is the same failure mode with no limit.

## Existing enforcement worth extending

- lib.rs deny list could add `clippy::print_stdout`, `clippy::print_stderr`
  (section 4), `clippy::panic` / `clippy::manual_assert` scoped to public API
  (section 5).
- A test-level check that README limit numbers equal the public consts.
- A typed `ErrorKind` for every limit (with `limit` fields) makes ad hoc
  `RuntimeError("... (limit 255)")` strings unnecessary; a gremlins-style text
  rule in brokkr could then reject `"limit 255"` literals.
