# Defect hunt: vm-core

Scope: `src/vm.rs`, `src/vm/object.rs`, `src/vm/object/collector.rs`,
`src/vm/anchor.rs`, `src/vm/global_env.rs`, `src/vm/lua_val.rs`,
`src/vm/metamethod.rs`, `src/vm/meta_name.rs`, `src/vm_aux.rs`, `src/host.rs`,
`src/error.rs`, `src/limits.rs`, `src/limits/tuning.rs`, plus the host
boundary bodies they own in `src/vm/eval.rs` (`call_unguarded`,
`invoke_rust_fn`, `fail_call`, admission) and `src/vm/stack.rs` (public
`push_*` / stack API).

Overall: the core invariants held up under the paths I traced. The GC root
set covers every place a live value can sit outside the heap graph (stack,
globals and suspended restricted envs, runtime literals, transient roots,
vararg records, anchors, event names, the string metatable, snapshot
baselines, open-upvalue pool slots); executing closures stay rooted in their
function slot, and the one way that slot could be overwritten (an open
upvalue aliasing a call window) is refused at runtime by
`InternalFault::WindowOverlapsOpenUpvalue`. Weak side tables (runtime caches,
`format_pointer_ids`, the pattern cache) are pruned or cleared at every
collection before any allocation can reuse a slot. Global-env versioning is
monotonic across `restrict`/`replace`, so no cache can see an ABA index.
Stack-cap preflights on every public push are in place, and the
key-held-in-a-Rust-local paths in the `__index` chain never allocate before
the key is pushed or the handler is entered. The findings below are what is
left.

## Findings

### 1. `on_error` fires for a swallowed nested error only if it came from a Lua frame

Claim broken: `HostCallbacks::on_error` (src/host.rs) is "Fired exactly once
per error, in addition to that error being returned from `call()`", and lists
"direct calls of non-callable values", `__call` chains and "result padding"
as covered.

How reporting is decided:

- An error that surfaces inside a Lua frame is reported unconditionally by
  `unwind_invocation` -> `report_frame_error` -> `host_error`.
- An error raised at a call *boundary* goes through `fail_call` ->
  `report_call_error`. That reports only when `is_outermost_host_boundary()`
  holds (`call_depth == 0 && active_rust_fn.is_none()`). Otherwise reporting
  is left to "the enclosing activation or callback", which reports only if
  the error propagates back to it.

So when a Rust callback calls `state.call(...)` and handles the `Err` instead
of returning it, the outcome depends on where the nested error came from:

- the callee is a Lua function that raises (`error()`, a type error, budget):
  `on_error` fires once;
- the callee is a non-callable value, a `__call` chain that hits
  `MetamethodDepthExceeded` or `TooManyArguments`, a Rust function that
  returns `Err`, a fixed result count whose nil padding overflows the stack,
  or the call is refused with `NativeCallDepthExceeded`: `on_error` never
  fires, although `call()` returned the error.

`tests/host_error_reporting.rs` only covers callbacks that propagate the
nested error (`rust_callback_propagating_nested_*`), where both kinds come
out at exactly one. There is no swallowing case.

Fix direction: decide which rule is the contract. "Report at the first
boundary that returns it to code that may continue" means `fail_call` reports
whenever it returns into a callback, not only at the outermost boundary;
`mark_reported` already makes later boundaries idempotent. "Report only what
reaches the host" means the Lua-frame unwind must stop reporting
unconditionally. Then make the `on_error` doc say which one it is.

### 2. Stack overflow at Lua-call admission is blamed on the callee's first line, depth overflow on the call site

`admit_lua_call` refuses a Lua call in two ways that render differently:

- `CallDepthExceeded` is checked before the activation record is pushed. The
  error goes back to the caller's frame and is located at the call
  instruction.
- `StackOverflow` from the frame reservation (`check_stack_space(grow)`)
  comes after the record is pushed. `reject_admission` builds
  `admitted_frame()` (ip 0) and reports through `report_frame_error`. The
  traceback's innermost entry, which `Display` uses for the `chunk:line:`
  prefix, is the callee at its first instruction's line. The attached source
  name (`host_error` reads `call_stack.last()`) is also the callee's chunk.

The script author sees `callee.lua:<first line of callee>: stack overflow`
for a function whose body never ran. The failing operation is the call in
the caller. `limits.md` says such a call "fails there with StackOverflow,
before its body runs". That locates the failure at the call, and the
rendered location contradicts it. This is the "misleading message" class
AGENTS.md counts as a defect.

Fix direction: report admission failures against the caller (pop the record
before reporting, or build the trace from the caller frame), so both
admission refusals render at the call site.

### 3. `open_libs` inside `with_restricted_env` leaves global state that the restore does not undo

`with_restricted_env` documents that "The original environment is restored
after the function completes (or errors)". If the host calls
`state.open_libs()` inside `f`, it:

- installs fresh library tables into the *restricted* `GlobalEnv`, which is
  discarded on restore;
- replaces `string_metatable` (and resets `string_mt_index_shape`)
  State-wide. That is not part of the env, so it is not restored, and
  `s:upper()` afterwards resolves through a string library table that no
  global names;
- with `snapshot`, has `capture_env_tokens` re-key `env_tokens` and
  `env_baselines` to the restricted env's new tables. After the restore the
  real `math`, `string`, `table` and `_G` objects no longer carry tokens, so
  a save classifies them as user tables, and the orphaned canonical objects
  stay rooted for the State's life.

Nothing forbids the combination, and both are public `&mut self` APIs.
Either refuse `open_libs` while `transient_roots.suspended_envs` is
non-empty, or document the combination as unsupported.

### 4. Stale contract text in this scope (code matches reality, doc does not)

- `src/limits.rs`, comment above `MAX_LOCALS`: "Operand value 255 is reserved
  for dynamic argument and result counts." The open-count sentinel is now
  `instr::OPEN_COUNT == 0xFFFF` on 16-bit operands. The 254 cap on
  `MAX_FIXED_ARGS` and `MAX_FIXED_RESULTS` now exists only because of the
  `u8` host count types and the `__call` receiver, not because of encoding.
  `src/instr.rs` repeats the stale rationale ("the shared 255 sentinel of
  the runtime count types"; `ArgCount` and `RetCount` have no sentinel
  value). `docs/limits.md` says "The dynamic-count sentinel is reserved",
  which no longer names a reason.
- `src/limits.rs` `MAX_FRAME_SLOTS`: "Maximum frame slots encoded by
  bytecode." Registers are 16-bit, so 255 is a compiler and verifier policy
  cap, not an encoding limit. The comment in `find_or_create_upvalue`
  (src/vm/eval.rs) that bounds the open-upvalue shift by it is still
  correct, because the verifier enforces it.
- `State.transient_roots` field doc: "Lua closure objects removed from the
  visible stack while they execute." No executing closure is held there any
  more; `mark_gc_roots` says so itself. It now holds
  `with_rooted_values`/`resolve_bytecode_runtime` temporaries and suspended
  restricted environments.
- `Closure::upvalues` doc (src/vm/object.rs): "the closure is cloned on every
  Lua-to-Lua call (`as_lua_function`)". Lua calls now go through
  `lua_function_bytecode` and `admitted_frame`, which clone three `Arc`s, not
  the closure. The `Arc<[_]>` choice is still right, for that reason.
- `GcHeap::mark` doc: "The upvalue_pool is needed to mark closed upvalues
  referenced by closures." `mark` takes no pool; `drain_mark_worklist` does.
- `GcHeap::as_lua_function_ref` carries two doc lines with
  `#[hotpath::measure]` between them. The first ("Get the object as a Lua
  function (closure)") belongs to `as_lua_function`.
- `with_metamethod_depth` doc: "A `__call` function handler is exempt (see
  `State::begin_call`)". The exemption is in `begin_callable_call`.

## Lateral observations (not defects against a stated claim)

- Performance: the pattern cache is cleared whole at every collection
  (`collect_garbage`), and the default policy collects at
  `max(2 * live, 20)` allocations. In allocation-heavy scripts the "compile
  once" cache is mostly cold. Pruning only entries whose `StringPtr` key
  died (as `format_pointer_ids` does with `val_is_live`) would keep the same
  safety argument, since keys are checked before any allocation can reuse a
  slot, and keep eviction deterministic.
- Performance: `sweep_bytecode_caches` builds a `Vec<Arc<Bytecode>>` of
  every reachable closure (refcount traffic per live closure) plus a
  `BTreeSet` per collection. Recording bytecode identities during
  `mark_children` would remove the second heap walk and the `Arc` clones.
- Performance: `as_lua_function` (an owned `Closure` clone, three `Arc`
  increments and decrements) is used in production only for `.is_some()`
  checks: `metamethod.rs` (`__index` and `__newindex` handler
  classification), `eval.rs` `begin_callable_call`, and `eval_index.rs`
  `index_string`. `as_lua_function_ref` is the right call. After that, the
  owned API has only test callers.
- Smell: `GcHeap::mark` and `StringPool::mark` silently ignore a vacant key.
  A stale root therefore shows up as a later "use-after-free detected"
  panic at the access site instead of at marking. A `debug_assert!` that
  roots resolve would move detection to where the root set is wrong.
- Smell: `State::pattern_compilations` is a `pub` method that exists only
  under `debug_assertions` (`#[doc(hidden)]`), so the public API surface
  differs between profiles.
- `with_restricted_env` silently scopes `seal_globals()` and
  `globals_sealed()` to the restricted env: a seal inside `f` is lost on
  restore. This is consistent with "bindings ... go away with that
  environment", but seal is documented as one-way and State-wide.
