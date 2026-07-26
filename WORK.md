# WORK.md

Current work item. Optimization loop 4 of the measured sequence.

---

## Target: SET_GLOBAL inline cache and allocation-free global writes

### The mechanism

`instr_set_global` (`src/vm/eval_store.rs`, near the `get_string_constant`
definition) does UTF-8 validation + `String` allocation + `Builtin::from_name`
re-check + IndexMap hash lookup on EVERY global assignment.
`instr_get_global` (`src/vm/eval_index.rs`) already has an IC: a
`GlobalLookupCacheEntry { globals_version, index }` per compiler-assigned
slot, validated by `globals_version`, hitting `globals.get_index` with zero
allocation. Writes have no equivalent.

### Measured evidence

`globals/write` (a designed probe; bench/write.lua is the seconds-scale twin)
at 5.6x lua5.5 (2026-07-25 README table, commit b4ae38e). `--hotpath` uuid
09074bd5 exists for distribution reading.

### Working design (verify each claim against code)

Mirror the read IC:

- Compiler: `assign_cache_slots` in `finalize` already rewrites
  GET_GLOBAL/GET_FIELD/SET_FIELD with real slot indices - extend it to
  OP_SET_GLOBAL, reusing the `global_lookup` slot family (RuntimeCaches
  already ships `global_lookup: Vec<GlobalLookupCacheSlot>`; a read and a
  write of the same name at different sites get different slots, which is
  fine). Verify what OP_SET_GLOBAL's A operand carries today - if it is a
  reserved-zero field, it can become the cache index.
- Runtime: on SET_GLOBAL with a slot, validate `globals_version` + cached
  index; on hit, write through `globals.get_index_mut(index)` - no UTF-8
  work, no String allocation, no hash. On miss, run today's path and
  repopulate the slot (only for existing non-builtin names; new-key inserts
  and builtin names take the slow path - note the parser emits SET_BUILTIN
  for statically-known builtin names, so the runtime `Builtin::from_name`
  check is cold-path only).
- IndexMap index stability: adding new globals appends (existing indices
  stable); nothing removes globals except environment swaps, which bump
  `globals_version`. Verify `set_global_value_owned` / `restrict_globals` /
  snapshot load for any path that reorders or removes entries WITHOUT
  bumping `globals_version` - any such path is a correctness bug for the
  READ IC already, so finding one is a finding in itself.

### Snapshot compatibility question (answer explicitly)

Saved bytecode is post-`finalize` and re-verified on load (`check_slots`
bounds cache indices). Old saves carry OP_SET_GLOBAL with whatever the old
A operand held (presumably 0) and old slot COUNTS:

- If A was reserved-and-zero-verified until now, old saves' SET_GLOBAL
  sites will all reference slot 0 under the new runtime. `Vec::get` on an
  out-of-range/unassigned slot must degrade to the slow path (today's
  behavior), never panic or alias incorrectly - aliasing slot 0 across
  sites is a warmth problem, not a correctness problem (validation is per
  access), but confirm the verifier's reserved-field check does not now
  REJECT old saves (a check that says "A must be zero" must be relaxed to
  "A must be a valid slot or zero"), and confirm new saves with nonzero A
  pass the slot-bounds check.
- State whether FORMAT_VERSION must bump. The strict-equality gate means a
  bump invalidates every existing save - avoid it if the operand
  reinterpretation is backward-safe as described. If it is truly needed,
  STOP and report rather than implementing.

### Constraints (inline; sessions read nothing else)

- Read and write code only; no cargo/brokkr/test/bench commands.
- Determinism: no HashMap/HashSet; identical behavior and identical
  `cost_used` (global writes' cost status must not change - check whether
  SET_GLOBAL charges today and preserve exactly).
- Clippy denies warnings; `unwrap_used` denied outside tests;
  `Result::ok()` banned.
- The write IC must respect every invalidation the slow path performs
  today: `globals_version` bumps, the `table_library_fallback` rebind hook
  (builtin writes only - confirm the cached fast path cannot carry a
  builtin name), and `_G` metatable writes (which go through
  `set_global_value_owned`, not the bytecode IC - confirm).
- RuntimeCaches is shared per (State, Bytecode) since commit b14fde8 -
  slots are per-site, shared across closures of one chunk; same
  correctness argument as the read IC.
- `src/compiler/verify.rs` `check_slots` and the reserved-operand checks
  must stay consistent with whatever encoding is chosen; extend the forged
  -bytecode tests for an out-of-range SET_GLOBAL slot.
- Keep `#[hotpath::measure]` on `instr_set_global`.

### Deliverable

Implementation plan: encoding choice, compiler/finalize changes, runtime
fast/slow path shape, verifier changes, snapshot-compat answer, test list
(slot forging, warm-hit semantics incl. env swap + builtin rebind + new-key
insert after warm hit, exact-cost pin), bench prediction (`write` collapses
toward `same_obj_write`-class ratios; nothing else moves; `cost_used`
identical).

---

## Agreed plan (consolidated 2026-07-26; implement exactly this)

The problem statement's "aliasing is only a warmth problem" claim is WRONG
and the design below exists to fix it: `GlobalLookupCacheEntry` stores only
`(globals_version, index)`, no name, so a legacy save's SET site
reinterpreted as slot 0 could hit an unrelated warmed READ entry and
overwrite the wrong global. Backward safety comes from a biased encoding
instead.

### Encoding

`OP_SET_GLOBAL`: `Bx` stays the string-literal index; `A` becomes:
- `0`: uncached - every legacy save, and post-capacity overflow sites.
- `1..=255`: cache index `A - 1` (slots `0..=254`).

`GET_GLOBAL` is untouched (its uncached sentinel stays 255). Add
`Instr::set_global_cached`, update the Debug formatting, and comment the
opcode-specific bias where the encoding is defined.

### Compiler (`assign_cache_slots` in `finalize`)

- GET_GLOBAL keeps its share-by-literal-index behavior.
- Each SET_GLOBAL site gets a FRESH slot from the same `global_lookup`
  family, encoded with the `+1` bias; when the shared 255-slot capacity is
  exhausted, later SET sites stay `A = 0`.
- Cached reads and writes count together into `global_cache_slots`.
- Recursion into nested chunks unchanged.

### Runtime (`instr_set_global`, keeps `#[hotpath::measure]`, stays cost-free)

Dispatch passes `inst.a()`. The handler:
1. Pops the value exactly once.
2. Decodes the slot with `checked_sub(1)` + `Vec::get`; zero/absent/
   out-of-range -> slow path.
3. Hit: `globals_version` must match, then `globals.get_index_mut(index)`
   updates only the value; a missing index falls back safely.
4. Miss/slow: UTF-8-validate from the bytecode literal. Builtin names
   ALWAYS go through the existing central setter (version bump +
   table_library_fallback rebind hook intact) and never populate the SET
   cache. An existing non-builtin updates in place via its found index and
   populates the slot (no String allocation, no second hash). A new
   non-builtin allocates the owned name via `set_global_value_owned` and
   leaves the site cold - the next execution populates.

### Verifier (`src/compiler/verify.rs` + save-side reverification)

- Remove SET_GLOBAL from the reserved-`A` group.
- `A = 0` accepted, consumes no slot. Nonzero decodes as `A - 1`, must be
  in `global_cache_slots` bounds AND match the next compiler-canonical
  global slot in the existing first-use validation stream (cached SET
  sites consume fresh positions; GET keeps deduplicating by literal).
- Final declared-count equality stays.

### Snapshot compatibility

`FORMAT_VERSION` stays 6, `COST_MODEL_VERSION` stays 2. Old saves' `A = 0`
sites stay uncached (cannot alias); their GET-only slot counts stay valid;
new saves with nonzero A pass the revised verifier. Old binaries cannot
read new saves (they still reserve A) - acceptable; the guarantee is new
binary reads old saves. The full-byte golden fixture WILL change: keep the
existing fixture as a legacy-v6 load-and-execute compatibility case, and
regenerate a current-output fixture for byte stability
(`tests/save_golden.rs`) - do not overwrite the only old-save evidence.

### Tests

- Compiler: GET dedup preserved; SET sites distinct; biased operands;
  builtin exclusion; 255 cached SET sites + an uncached 256th.
- Forged saves: legacy `A = 0` accepted with zero slots; `A = 1` accepted
  with one slot; out-of-range and wrong-first-use-order SET slots
  rejected; a legacy `A = 0` SET coexisting with a warmed unrelated GET
  slot cannot overwrite the GET's global.
- Warm-site semantics: one SET site executed 3+ times (insert, populate,
  hit); alternating closures sharing one (State, Bytecode) runtime.
- Env swap: warm a writer, run it restricted, restore, confirm
  repopulation against each version with no cross-environment leak.
- Builtin rebind: warm a non-builtin writer, rebind `table`, confirm hook
  + version bump + writer repopulation; internal forged-builtin SET test
  proving builtin sites never cache.
- Append stability: warm `x`-writer, insert new `y` (including via `_G.y`
  proxy), write `x` again, both correct.
- Exact-cost pin: three-addition writer stays exactly `cost_used == 3`.
- New-save round-trip with nonzero SET operands + retained legacy fixture.

### Bench acceptance (orchestrator)

`write` collapses toward cached-write-class ratios; nothing else moves
materially; `cost_used` byte-identical everywhere.
