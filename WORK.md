# WORK.md

Current work item. This is an optimization loop, not a bug fix: the target is
measured, the fix must not change observable script behavior without an
explicit decision, and the verdict is an interleaved A/B benchmark, not a test
going green.

---

## Target: gate the table-library fallback off the field-read miss path

### The mechanism

Every plain-table field read that misses takes a table-library fallback lookup
before returning nil. `instr_get_field` (`src/vm/eval_index.rs:16-66`):

- receiver has **no metatable** and the key is absent -> line 41:
  `push_table_library_field(key, local_cost)`
- receiver has a metatable, `__index` resolution returned nil -> line 56:
  same fallback

`push_table_library_field` (`eval_index.rs:382-395`) does
`get_global("table")` (a Builtin array read - cheap) plus a full
`get_table_with_key` probe against the library table, plus the stack traffic
around it. The feature it serves is method sugar on plain tables:
`t:insert(x)` resolves `insert` through the `table` library when `t` has no
such field. But the fallback fires for EVERY missing key, and
`if t.optional_field then` is among the most common patterns in real script
code.

### Measured evidence (2026-07-26, plantasjen)

- `--hotpath` on `examples/fields/miss.lua` (uuid 170db9ef, commit c4a303b):
  `push_table_library_field` is 27.3% of the whole run, 2,163,000 calls at
  100ns avg; the enclosing `instr_get_field` is 58.6%.
- `--bench` verdict workload `miss` (bench/miss.lua): 5.3s standalone;
  ratio vs reference: `fields/miss` 6.6x lua5.5 against
  `fields/same_obj_read` 3.9x (2026-07-25 README table, commit b4ae38e) - a
  missing field costs ~1.7x a hit in relative terms.

The bench workload `miss` is a diagnostic probe (header comment in the file
says what it isolates); the win on plausible code is smaller but the pattern
is ubiquitous.

### The key fact making a gate possible

The fallback can only ever resolve keys that are members of the `table`
library. That name set is known statically at VM build time
(`src/lua_std/table.rs` installs them: insert, remove, concat, sort, unpack,
pack, move, ...read the file for the exact list). For any other key the
fallback is a guaranteed nil - pure wasted work.

### The semantic trap the audit missed

`push_table_library_field` resolves against the **current** `table` global at
call time. Scripts (or hosts) can mutate it:

- `table.mymethod = function(t) ... end` then `t:mymethod()` resolves through
  the fallback today.
- `table = something_else` (rebinding the global; note `table` is a `Builtin`,
  so check how SET_GLOBAL/SET_BUILTIN interacts with the builtins array).
- `with_restricted_env` swaps the whole global environment.
- Snapshot save/load reconstructs globals; the loaded `table` may be an
  extended one.

A purely static gate ("key not in the compiled-in name list -> skip fallback")
changes observable behavior for all of those. The plan must either:

(a) preserve semantics exactly - static fast-reject PLUS a cheap validity
    check that detects "the table library is not in its pristine state" and
    falls back to today's path (identity of the `table` global value +
    `Table::version` of the library table are the obvious ingredients; work
    out where to cache them and what invalidates the cache - note
    `globals_version` exists for the global ICs, check exactly when it bumps),
    or
(b) declare the restriction (fallback sugar only resolves the built-in name
    set) as a deliberate contract change - which is a product decision the
    orchestrator must sign off on, not something to decide inside the session.

Prefer (a) unless it costs the fast path something measurable; flag (b) only
if (a) turns out structurally ugly.

### Design options to weigh

1. **Runtime gate, interned-pointer compares.** At State init (or lazily),
   intern the lib method names and hold their `StringPtr`s (strings are
   interned per-State, so pointer equality is content equality). On the miss
   path, compare the key's `StringPtr` against the ~10 pinned pointers before
   taking the fallback. No compiler changes, no bytecode changes. Cost: up to
   ~10 u64 compares per miss (vs today's global read + table probe + stack
   traffic). The pinned pointers must survive GC (string interning + GC sweep
   of unused strings - verify pinned literals are rooted, or root them).
2. **Compile-time site splitting.** The compiler knows each GET_FIELD's key
   literal. Emit the fallback-capable path only for sites whose literal is in
   the static name set (a flag bit or an opcode variant); all other sites
   skip the fallback with zero runtime checks. More invasive: touches
   codegen, the opcode surface, `analyze_cost`, the phase-1 bytecode
   verifier, and interacts with the snapshot format (`verify.rs` knows the
   opcode list). Same semantic trap, same validity-check need at the
   surviving sites.
3. **Method-call-position-only fallback.** Rejected unless review finds
   otherwise: `local f = t.insert` resolves through the fallback today, so
   restricting to method position is a behavior change of shape (b) with
   extra compiler work on top.

Option 1 is the working hypothesis: smallest diff, no new surfaces, and the
per-miss cost drops from a table probe to a handful of compares.

### Cost-model question (needs an explicit answer in the plan)

The fallback path charges cost today (`local_cost` flows into
`get_table_with_key`). Skipping it makes miss reads cheaper in `cost_used`,
which re-bases the cost fingerprint of any script with misses - same
replay-versioning consideration as constant folding (see OPTIMIZATIONS.md).
Decide: charge-as-before (keep the fingerprint stable, forgo the cost-side
honesty) or let cost reflect actual work (note it in the commit as a
fingerprint change). State which and why in the plan.

### Constraints (restated inline; do not go read other process docs)

- Do NOT run `cargo`, `brokkr`, or any build/test/bench command. Read and
  write code only; the orchestrator validates and benchmarks between steps.
- Determinism is a product requirement: no HashMap/HashSet (IndexMap/BTreeMap
  only), no unseeded RNG, identical source must produce identical behavior
  and identical `cost_used` for a given VM version.
- Clippy denies warnings; `unwrap_used` denied outside `#[cfg(test)]`
  (`expect` with an invariant-explaining message is allowed); `Result::ok()`
  banned; `dbg_macro`/`todo!` denied.
- The stack cap is a real invariant: any new push path must be checked,
  preflighted, or provably net-neutral (see the comment discipline in
  `eval_index.rs` - pushes annotated against the pops that balance them).
- `push_table_library_field` is `#[hotpath::measure]`-annotated; keep a
  measurement point on whatever the fallback becomes. Do not annotate any
  function whose frame stays live across the recursive bytecode dispatch.
- Behavior changes require the orchestrator's sign-off (see the semantic
  trap above). The default is exact semantic preservation.
- The examples/ tree is a test surface (run_examples + differential gate vs
  reference Lua). `t:insert(...)` sugar and fallback-miss behavior are
  observable there; any new test scripts must print `<name>: true` and
  diff clean against lua5.2/lua5.4 - but note reference Lua does NOT have
  this fallback (plain `t.insert` is nil there), so fallback-specific tests
  belong in Rust integration tests, not diffed examples.

### Reading list (all paths relative to repo root)

- `src/vm/eval_index.rs` - `instr_get_field`, `push_table_library_field`,
  `get_string_table_field` (the string twin - NOT in scope, but the gate
  must not accidentally break it), the field IC helpers.
- `src/vm/eval_store.rs` - SET-side twin `try_insert_table_direct` for the
  stack-discipline comment conventions.
- `src/lua_std/table.rs` - the authoritative name set and how it installs.
- `src/vm/object.rs` - string interning (`alloc_string`, `find_by_hash`),
  GC sweep of strings, what pins a `StringPtr`.
- `src/vm.rs` - `globals_version` semantics, `builtins` array, `set_global`
  paths, `with_restricted_env`.
- `src/vm/table.rs` - `Table::version` bump rules (documented at the field).
- `tests/` - existing integration suites for shape; `examples/` fallback
  usage (grep for `:insert(` / `:remove(`).

### Deliverable

An implementation plan: chosen option, the validity-check design with its
exact invalidation triggers, the cost-model answer, the diff's file-by-file
shape, the test list (Rust-side + any example updates), and the bench
prediction (which workloads move, which must not move - `same_obj_read` and
`polymorphic` are the hit-path guards).

---

## Agreed plan (consolidated 2026-07-26; implement exactly this)

Option 1, runtime gate, exact semantic preservation. Two corrections to the
problem statement above, both verified: (1) the ordinary fallback charges NO
cost today - `GET_FIELD` and raw table reads are free and `get_table_with_key`
only forwards `local_cost` into metamethod-driven Lua calls - so the gate must
not charge anything and `cost_used` fingerprints must not move; (2) the gate
logic is inverted from the sketch: MEMBER keys always take today's fallback,
and the validity guard exists only to prove a pristine library table cannot
contain a NON-member key.

### The cache

`TableLibraryFallbackCache`, an `Option<_>` field on `State`, captured in
`vm_aux.rs` immediately after `lua_std::open_libs` succeeds (before snapshot
environment capture), and only when the installed `table` value is an actual
table with no metatable:

- the canonical library `ObjectPtr`
- the seven method-name keys read from the installed table itself (not a
  hardcoded list, so the gate cannot drift from the registration), held as
  rooted `Val::Str`
- a conservative shape guard: `Table::version` + physical storage-slot count
  + dead/tombstone count + metatable identity (`Option<ObjectPtr>`, `None` at
  capture)

The name `Val`s join `mark_gc_roots`. The canonical `ObjectPtr` is
deliberately NOT rooted by the cache; setter invalidation (below) plus
generational slotmap keys keep a stale pointer from ever being dereferenced -
it is compared first, and a freed/reused slot compares unequal.

### The gate, on every fallback request (`push_table_library_field`)

1. Key pointer equals one of the seven cached names -> execute today's
   fallback body unchanged. This preserves `t:insert(x)`,
   `local f = t.insert`, replaced members, and deleted members exactly.
2. Otherwise validate: current `Builtin::Table` value is the cached object,
   the object is still a table, and its shape guard equals the capture.
3. Valid -> preflight TWO stack slots (the high-water mark of today's
   fallback, preserving the exact overflow boundary), then push nil.
4. Any check fails, or no cache -> today's body unchanged.

Keep the `#[hotpath::measure]` annotation on `push_table_library_field` so
call counts stay comparable while the mean falls. Do not touch
`get_string_table_field` (the string twin).

### Invalidation triggers (each one explicit in code)

- `open_libs`: recapture, replacing any previous cache.
- Any write that can rebind the `table` global - the shared global setter
  (host `set_global`, `_G` writes, general `SET_GLOBAL`) and
  `instr_set_builtin` - drops the cache when the new value is not the cached
  canonical object; reassigning the same object may keep it.
- Shape drift is caught by the guard itself: added key -> slot count;
  deleted key -> dead count; compaction/reorder -> version; metatable
  install -> metatable identity. Value-only replacement of an existing entry
  changes nothing and needs no invalidation (member keys bypass the guard;
  a non-member key cannot already exist in a pristine table).
- `with_restricted_env`: do NOT drop on swap - the identity check rejects
  the gate while restricted (builtins swapped) and it becomes valid again on
  restore. The suspended environment roots the object meanwhile.
- Snapshot load: ordinary env deltas are caught by guard/setter paths, but
  explicitly drop the cache before an ordered `"table"` environment delta
  runs `clear_and_insert_entries` (`save_state.rs` load path) - a wholesale
  rebuild of the same object could coincidentally restore pristine-looking
  shape fields.

### Behavior explicitly preserved (test each)

- All seven names resolve via `t.<name>` and `t:<name>(...)` on plain tables.
- `local f = t.insert; f(t, x)` works.
- `table.insert = replacement` is observed by the fallback.
- `table.custom = f` resolves via `t.custom` and `t:custom()` (guard
  invalidated by slot count).
- Rebinding `table` to a replacement table resolves its members; rebinding
  to a non-table preserves today's error (yes, the error - see the semantic
  trap section; preserving it is deliberate).
- A metatable on the canonical table resolves unknown keys via `__index`.
- `with_restricted_env` without `table` whitelisted preserves today's error;
  behavior restores after.
- Rebind + forced GC never touches the stale cached object.
- A pristine-table unknown miss has byte-identical `cost_used` (exact-cost
  regression test, so nobody accidentally charges the gate later).
- Snapshot round-trips preserve extension, rebind, metatable fallback, and
  the ordered-replay case.

### File-by-file

- `src/vm/table.rs`: compact shape-guard accessor + an init-only accessor
  returning the table's live string-key values; unit tests that append,
  tombstone-delete, compaction, and metatable install each break pristine
  validation.
- `src/vm.rs`: the cache struct + `Option` field (init `None`), capture and
  rebind-invalidation helpers, rooting in `mark_gc_roots`, hook in the
  shared global setter.
- `src/vm_aux.rs`: capture after `open_libs`.
- `src/vm/eval_index.rs`: the gate in `push_table_library_field`;
  `instr_set_builtin` invalidation.
- `src/vm/save_state.rs`: explicit drop before ordered `"table"` env replay.
- New `tests/table_library_fallback.rs`: the semantic/GC/restricted-env/cost
  list above.
- `tests/save_state.rs`: the four snapshot regressions.

No compiler, opcode, verifier, analyze_cost, stdlib-registration, example,
bench-script, registry-pin, or snapshot-format changes.

### Bench prediction (orchestrator verifies after the build)

`fields/miss` improves materially (fallback was 27.3% of the instrumented
run; 15-25% wall reduction plausible). `same_obj_read` and `polymorphic`
must stay within noise. No `cost_used` may move on any workload.
