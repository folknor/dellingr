# Bug hunt 2026-07-24 - Corner D: State lifecycle, persistence, host surface

Auditor scope: State construction/configuration, cost budget API and
consume_cost, restrict_globals/with_restricted_env, GC-root enumeration and
collection triggering, RNG, snapshot encoder/decoder, error types,
HostCallbacks, CLI. Read-only audit; repros below are written down, not
executed.

Files read: src/vm.rs, src/vm/save_state.rs, src/vm/rng.rs, src/error.rs,
src/host.rs, src/main.rs, src/vm/tests.rs, plus supporting context:
src/vm/{object,stack,anchor,eval,eval_index,frame,table}.rs, src/lib.rs,
src/vm_aux.rs, src/lua_std.rs, src/lua_std/{basic,math}.rs, src/instr.rs,
Cargo.toml, README.md, OPTIMIZATIONS.md, TODO.md.

---

## Findings (most severe first)

### D1. HIGH - `with_restricted_env` un-roots the saved environment; GC during the closure frees it, permanently poisoning the State

`src/vm.rs:614-654`. `with_restricted_env` moves the real environment into
locals (`saved_globals`, `saved_builtins` via `std::mem::replace`) and installs
a whitelist-only environment. While `f` runs, those saved values are NOT GC
roots: `mark_gc_roots` (vm.rs:69-89) marks only `stack`, `globals`, `builtins`,
`string_literals`, `active_call_roots`, `registry` - the swapped-out originals
are invisible. (`env_tokens` is also not a root, so it does not accidentally
save the library tables either.)

Any allocation inside `f` can trigger a collection: `alloc_string`
(vm.rs:657-665), `push_closure` / `initialize_frame` (src/vm/eval.rs:424-457)
all do `if self.heap.is_full() { self.gc_collect(); }`, and with
`GC_INITIAL_THRESHOLD = 20` plus the adaptive 2x-survivors threshold, real
scripts hit this quickly. The collection then frees every object reachable only
from the saved environment: the `math`/`string`/`table` library tables, the
`_G` proxy and its metatable, and every non-whitelisted user global that is an
object. After the restore, `state.globals` / `state.builtins` contain stale
`ObjectPtr`/`StringPtr` keys. The slotmap catches the use-after-free, but as a
panic: `GcHeap::get` / `StringPool::get` (src/vm/object.rs:221-234, 574-579)
`expect("Invalid ObjectPtr: object was freed (use-after-free detected)")`.

Failure scenario (repro for the orchestrator):

```rust
use dellingr::{ArgCount, RetCount, State};
let mut state = State::new();
state.load_string("t = {1, 2, 3}").unwrap();
state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();

state.with_restricted_env(&["print"], |s| {
    // Enough churn to cross the GC threshold at least once.
    s.load_string(
        "local a = {} for i = 1, 200 do a[i] = 'x' .. i end print(a[1])",
    ).unwrap();
    s.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap();
});

// Any later touch of a collected env object panics with
// "Invalid ObjectPtr: object was freed (use-after-free detected)":
state.load_string("print(t[1], math.floor(1.5))").unwrap();
state.call(ArgCount::Fixed(0), RetCount::Fixed(0)).unwrap(); // panic
```

The recently added panic-guard (L11) restores the environment on unwind, but it
restores dangling pointers; the guarantee "restored after the function
completes (or errors)" is hollow whenever a GC ran inside `f`.

Note there is no test coverage for restricted env + GC in src/vm/tests.rs.

Fix sketch: make the saved environment a root while swapped out. E.g. a
`State` field `suspended_env: Vec<(IndexMap<String, Val>, [Val; Builtin::COUNT])>`
(a Vec so nesting works) that `with_restricted_env` pushes/pops and that
`mark_gc_roots` marks, keeping "single source of truth" honest. Alternative
(worse): snapshot-and-restore the GC threshold to `usize::MAX` during `f`,
which merely defers the problem to an explicit `gc_collect`.

### D2. HIGH (hostile input) - `load_state` performs zero bytecode validation; a forged save escalates from `LoadError` to process panic

`src/vm/save_state.rs`: the codec is carefully bounds-checked (length caps,
`read_exact`, no over-reservation) but the *semantic* content is trusted.
`materialize_bytecode` / `build_bytecode` (save_state.rs:645-709) rebuild
`Bytecode` directly from attacker-controlled fields via `Instr::from_raw`
(src/instr.rs:350) with no verifier. Game saves are classic user-edited input,
so this is reachable in the product's own use case.

Concrete panic vectors, all reachable by handcrafting a `DLGS` v2 payload whose
closure bytecode violates compiler invariants (each is an ordinary safe-Rust
panic, not UB, but it aborts the host's callback thread and is not catchable as
`LoadError`):

- code that runs off the end (no `OP_RETURN`): `Frame::get_instr` indexes
  `code[self.ip]` (src/vm/frame.rs:103-107) - index OOB panic.
- `OP_PUSH_NUM`/`OP_PUSH_STRING`/`OP_CLOSURE` with an index beyond
  `number_literals`/`string_literals`/`nested` (frame.rs:110-117,
  eval_index.rs:371) - index OOB panic.
- `OP_GET_LOCAL`/`OP_SET_LOCAL` beyond the frame (eval_index.rs:436-440)
  - index OOB panic; `OP_GET_UPVALUE n` with no upvalues (eval_index.rs:443)
  - index OOB panic.
- `OP_GET_BUILTIN`/`OP_SET_BUILTIN` with slot >= `Builtin::COUNT` (19)
  (eval_index.rs:414-423, `builtins[slot as usize]`) - index OOB panic.
- `OP_POP`/`OP_SWAP`/`OP_DUP` on an empty stack - `pop_val` expect / swap OOB.
- forged `num_locals`/cache-slot counts inconsistent with cache-indexed
  instructions.

Also forgeable as plain data: `cost_remaining`/`cost_budget`/`cost_used` (a
save-file editor grants itself infinite budget) and `rng_state`. Those may be
acceptable (saves are host-owned data), but the panic class is not, given the
decoder otherwise promises graceful `LoadError`s for corrupt input
(`CorruptArena`, `DecodeError`).

Fix sketch: a linear verifier pass over each `SavedBytecode` at load time -
opcode is in the known set, literal/nested/cache indices within declared pools,
jump targets within `0..=code.len()` from every reachable ip (a simple
per-instruction static check of `ip + sbx` suffices given `Frame::jump`
re-checks), code non-empty and terminated, `upvalues` descriptor indices within
parent ranges, `num_locals`/`num_params` sane. Alternatively, document loudly
that save bytes are trusted input (weaker; contradicts the "hostile-input"
posture of the decoder).

### D3. HIGH (hostile input) - `build_bytecode` recursion is unbounded; a forged save aborts the process via native stack overflow

`src/vm/save_state.rs:657-709`. `build_bytecode` recurses through `nested`
chunk ids with cycle detection (`visiting`) but no depth bound. A forged save
can encode N chunks in a linear parent-child chain (each `SavedBytecode` with
one nested id, a few dozen bytes each), so a ~10 MB file yields a recursion
hundreds of thousands of frames deep. Native stack overflow aborts the
process; it cannot be caught or returned as `LoadError`. Legitimate saves are
bounded by compiler nesting limits, so an explicit depth cap (or an iterative
post-order worklist) costs nothing for real data.

### D4. MEDIUM (script-triggered host abort) - `SaveBuilder::encode_object` recursion is unbounded in data depth

`src/vm/save_state.rs:313-351`. `encode_object` -> `encode_val` ->
`encode_object` recurses per nesting level of tables/closures. A script can
build a deep chain cheaply (`local t = {} ; for i = 1, 200000 do t = {t} end ;
g = t` costs ~2 per iteration, well inside normal budgets). The host then calls
`save_state()` and overflows the native stack - script-controlled data kills
the host process during an API call that is typed to return
`Result<_, SaveError>`.

Cross-corner note (GC): `GcHeap::mark` / `mark_children`
(src/vm/object.rs:355-386) has the same recursive shape, so with auto-GC
enabled the same chain usually aborts inside `gc_collect` even before the save
is attempted. Both should move to an explicit worklist together (see D-OPT-1).
Flagging here because save/collect triggering are in this corner; the marking
rewrite belongs to the GC corner.

### D5. MEDIUM (round-trip fidelity) - user mutations inside environment tables are silently dropped by save/load

`src/vm/save_state.rs:303-310`: when the walker meets an `ObjectPtr` present in
`env_tokens` it emits an `EnvObj` token and never walks the table's entries.
Consequently `math.myconst = 42`, `string.trim = function(s) ... end`,
`table.foo = {…}` (a supported, ordinary Lua idiom - extending library tables)
survive in the live State but vanish on save/load: on load the tokens resolve
to the freshly rebuilt pristine libraries. Values reachable ONLY through an env
table (e.g. that `table.foo` subtable) are dropped entirely, with no
`SaveError` and no diagnostic. README's snapshot section ("globals, reachable
tables/closures/upvalues/strings ... are persisted") does not carve this out;
the module doc only covers the reverse direction (new build adding `math.foo`).

Fix options: (a) at save time, diff each env table against a capture-time
pristine snapshot (entry list captured alongside `env_tokens`) and persist the
delta, replayed on load after `open_libs`; (b) cheaper: detect a modified env
table (its `version()` differs from capture time, or entry-count/name diff) and
fail fast or surface it in `SaveDiagnostics`, plus document the limitation in
README and the module doc. Doing nothing silently loses user state.

### D6. MEDIUM - `%p` identity state is not persisted; identities collide across save/load

`src/vm.rs:164-168` (`format_pointer_ids`, `next_format_pointer_id`) are absent
from `SavePayload` (save_state.rs:198-210). After a load, the counter restarts
at 1 while strings produced by `string.format("%p", x)` before the save can
persist in saved globals. A new object formatted after load can therefore
render byte-identical to a different pre-save object's `%p` string, breaking
the uniqueness that deterministic `%p` ids exist to provide, and an
uninterrupted run diverges from a save/load-interrupted run (replay-affecting
if scripts branch on `%p` output). Fix: persist `next_format_pointer_id` and
the id entries whose `Val`s are reachable in the payload (dead entries can be
dropped - consistent with the TODO.md pruning sketch), or document that `%p`
identities are per-process and never comparable across a load.

### D7. LOW (host API) - `set_top` growth bypasses `MAX_STACK_SIZE`; assert-vs-Result inconsistency in stack API

`src/vm/stack.rs:25-57`. `set_top(i)` with a large positive `i` pushes nils in
a loop with no `check_stack_space`, so a host (or a buggy RustFunc) can request
`set_top(isize::MAX)` and OOM the process, bypassing the documented 1M-value
cap enforced everywhere else. Also inconsistent error contract on the same
surface: `set_top` and `pop` `assert!` (panic) on misuse while `insert`,
`remove`, `replace`, `push_value` return `ErrorKind::InvalidStackIndex`. Given
scope item 7 (error paths must leave State consistent and reusable), the
panicking forms are the odd ones out. Suggest `check_stack_space` in the growth
arm and converting the asserts to `InvalidStackIndex` errors pre-1.0.

### D8. LOW (docs vs code) - MSRV mismatch

`Cargo.toml:5` says `rust-version = "1.97"`; README badge (line 5) and
AGENTS.md both say 1.92. One of them is wrong; snapshot/versioning doc-audit
item. (README's `dellingr = "0.2"` snippet also trails the crate's 0.3.0, minor.)

### D9. LOW (docs) - TODO.md "Stable RustFunc identity for serialization" is stale

TODO.md:79-98 claims "dellingr doesn't ship a serialization story today" and
proposes registration-time stable ids. The snapshot feature ships exactly this
(`State::register_rust_fn`, `set_global_named_rust_fn`, dotted stdlib ids,
save-side `rust_fn_ids_by_addr`). Per the backlog convention ("entries get
deleted as they ship"), the entry should be deleted or rewritten to cover only
the remaining sliver (pointer-address rendering in `Display`/hash of `Val`).

### D10. LOW (cross-corner: error taxonomy) - script `error()` raises `ErrorKind::InternalError`

`src/lua_std/basic.rs:140-147` implements the `error` builtin with
`ErrorKind::InternalError(message)`. `src/error.rs:97-99` documents
`InternalError` as "corrupt bytecode or VM bug ... report these as bugs", and
`Display` renders it as `internal error: <msg>`. So `error("oops")` in a script
surfaces as an internal VM bug ("0:0: internal error: oops") - wrong taxonomy
for host telemetry (an embedder filtering `InternalError` for crash reporting
gets user-raised errors), and diverges from reference Lua's
`input:LINE: oops` rendering. `RuntimeError` (or a dedicated `ScriptError`
variant carrying the position prefix like reference Lua) is the right shape.
Files span this corner (error.rs) and the stdlib corner (basic.rs).

### D11. LOW (snapshot) - anchors created inside the `load_state` setup closure are silently invalidated

`src/vm/save_state.rs:599`: `materialize_payload` ends with
`state.registry.clear()`, which runs AFTER the host's `setup` closure
(save_state.rs:459). A host that pushes a value and anchors it during setup
gets a handle that is stale the moment `load_state` returns, with no error
(later use returns `InvalidAnchor`). Either run `registry.clear()` before
`setup`, or document that setup-time anchors do not survive. (Clearing exists
to drop anchors inherited from `State::with_callbacks`; there are none in a
fresh State, so moving the clear earlier is free.)

---

## Re-scrutiny of recently fixed items (all verified against code)

- Saturating `consume_cost` (vm.rs:329-339): correct. `saturating_sub_unsigned`
  / `saturating_add`; boundary contract "the op that pushes you over completes,
  the next costed op fails" holds, including for `u64::MAX` host charges.
  Covered by `consume_cost_saturates_large_host_charges` (vm/tests.rs:9-37).
  The eval-loop batcher (frame.rs:147-159) flushes eagerly when
  `cost_remaining - pending <= 0`, so batching cannot defer the boundary by up
  to 63 ops; charging happens before the op executes. Consistent.
- Empty-state snapshot round-trip: `has_standard_environment` false path,
  `State::empty_with_callbacks` on load, no env tokens - correct.
- on_error for host-direct RustFunc failures (eval.rs:95-99): fires exactly
  once (`call_depth == 0 && stack_trace.is_empty()` guard); Lua-frame path
  fires in `eval_closure_inner` when attaching the trace. Matches the host.rs
  doc contract.
- Panic-safe restricted-env restoration (vm.rs:643-653): the catch_unwind
  guard is correct as far as it goes, but see D1 - restoration can restore
  dangling pointers.
- Documented contracts: math determinism doc (math.rs:1-11, README) matches
  code (`state.rng` only; transcendental caveat accurate). Snapshot versioning
  doc (L10) matches code: FORMAT_VERSION strict equality is the gate, crate
  version string is read-and-discarded. `analyze_cost` "neither lower nor upper
  bound" doc matches `ScopeCost::analyze_chunk`. The one mismatch found is D8
  (MSRV).

Other checks that came up clean: `VmRng` is fully deterministic and pinned by
test; seed 0 default documented; `random_range_i64` degenerate-range and bias
behavior documented and sane. Anchor registry generational + state-id checks
are sound; `call_anchor` correctly rejects `ArgCount::Dynamic`. Save output is
deterministic (BTreeMaps + traversal order + insertion-order globals; aliased
fn addresses resolve to the lexicographically-smallest id independent of
registration order). Decoder reservation capping (`len.min(remaining)`) defangs
forged length prefixes; memory use is linear in input size. `validate_quiescent`
covers all transient stacks/counters; eval_closure watermarks (L8) keep the
State quiescent after killed callbacks. CLI arg handling is fine modulo nits
(multiple filename args: last silently wins; negative `--limit` accepted and
immediately exhausts - both arguably fine).

---

## Optimization opportunities (structural first)

### D-OPT-1. Iterative graph walks: GC mark + save walker + bytecode rebuild (coherent rewrite, removes an abort class)

One rewrite kills D3/D4 and the GC-marking cousin: replace the three recursive
graph walks (`GcHeap::mark_children`, `SaveBuilder::encode_object`,
`build_bytecode`) with explicit worklists (Vec-based stack of pending
ObjectPtrs / chunk ids, iterate until empty). Determinism is unaffected
(traversal order can be made identical to today's recursion by pushing children
in reverse). Marking via worklist also tends to be faster than deep call
recursion for big heaps (no per-object call frame, better locality), so this is
a perf win in `gc_collect` - one of the two `#[hotpath::measure]` internal hot
paths - not just a robustness fix. This is the "do not preserve structure just
because it exists" case: the recursion is only there because it was easy.

### D-OPT-2. Save walker: drop eager path-string construction (local, O(depth^2) -> O(1) on success)

`encode_object`/`encode_val` (save_state.rs:313-351) build
`format!("{path}[{idx}]")` breadcrumbs for EVERY value visited, purely to
populate `SaveError::UnregisteredFunction.reachable_from` on the rare failure.
For a nested structure the accumulated string length grows with depth, so the
success path does O(total-values x depth) string allocation and copying. Track
a cheap breadcrumb instead (e.g. a `Vec<(parent_object_id, entry_index)>` scope
stack, or just the current object id), and reconstruct the human-readable path
only when actually building the error. Combined with D-OPT-1's worklist this
falls out naturally (the worklist entry carries the parent link).

### D-OPT-3. Save walker: key string dedupe by `StringPtr`, not by content bytes (local)

`encode_val` for `Val::Str` (save_state.rs:281-292) copies the string content
(`to_vec()`), then probes `BTreeMap<Vec<u8>, u32>` (full-content comparisons on
every probe), then clones the bytes again for the id map. Strings are interned
per-State, so `StringPtr` equality is content equality: key the dedupe map as
`BTreeMap<StringPtr, u32>` (StringPtr is Ord-able via its key data, or use the
existing `Ord` derive route as ObjectPtr does) and copy the content exactly
once, when first appending to `strings`. Saves one full copy plus all content
comparisons per string occurrence; ids still assigned in first-encounter order,
so byte-identical output.

### D-OPT-4. `with_restricted_env` restricted-builtin rebuild is fine; the fix for D1 should carry the cost

Whatever shape the D1 fix takes, prefer the rooted-field variant over cloning:
pushing the saved env into a marked `State` field is O(1) and keeps
`mark_gc_roots` as the single source of truth, versus deep-copying the
environment (O(globals)) or suppressing GC (unbounded heap growth during `f`).
Mentioned so the fix does not get implemented as "clone everything".

### D-OPT-5. Cross-corner note: `UpvaluePool` never frees slots; contradicts the long-lived-State story

`src/vm/object.rs:51-53` justifies never freeing upvalue slots with "VMs have
short lifetimes", but this corner's snapshot feature exists precisely for
long-lived, save/load-cycled States, and the product runs continuous per-tick
callbacks. Every closure-with-captures created over a session leaks a pool slot
forever (`UpvaluePool::alloc` only grows). A long game session with closure
churn grows without bound; ironically a save/load round-trip compacts the pool
(only reachable closed upvalues are serialized). Sketch: sweep the pool during
`gc_collect` - mark reachable `UpvalueRef`s while marking closures (the
infrastructure already threads `upvalue_pool` through marking), then thread
freed indices into a free list consumed by `alloc`. Determinism holds because
allocation order stays a pure function of execution history. Belongs to the
GC/object corner for implementation; recorded here because the leak profile is
a State-lifecycle concern.

### D-OPT-6. Snapshot encoder buffer pre-sizing (micro, take-or-leave)

`Encoder::new` starts from an empty Vec; a save of a large world reallocates
the output buffer log-many times. A one-line `Vec::with_capacity` seeded from a
cheap estimate (e.g. `strings total bytes + 16 x value count`) removes the
churn. Only worth bundling with other snapshot work.

---

## Non-findings (checked, deliberate or acceptable)

- Anchored-only values not being serialized: documented, surfaced via
  `SaveDiagnostics::anchor_count`.
- `pairs(_G)` yielding nothing (proxy table is empty): design consequence of
  the `_G` metatable proxy; noted for the stdlib corner, not pursued here.
- `rust_fn_ids_by_addr` collapsing ICF-folded functions to one id: safe -
  folding requires identical code, so either id resolves to identical behavior
  in any build compiled from the same source.
- Cost/rng values in the save being host-forgeable: saves are host-owned data;
  the integrity problem is the panic class (D2), not the data trust.
- `error()` lacking level/position arguments, no `pcall`: Won't-implement list.
