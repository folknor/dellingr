# WORK.md

Current work item. Optimization loop 5 of the measured sequence.

---

## Target: version-validated cursor for `pairs` iteration

### The mechanism

`instr_tfor_call` (`src/vm/eval_control.rs:128`) already fast-paths the
builtin `next` by fn address into `instr_tfor_call_next` (line 187), which
calls `Table::next(&control)` - a full hash probe (`get_index_of`) of the
control key on EVERY iteration. `pairs` over an N-entry Map table is N hash
probes. Measured: `iter/pairs` at 2.0x lua5.5; `--hotpath` (uuid 625433ea):
`instr_tfor_call` 31.5% + `Table::next` 10.8% + `instr_tfor_loop` 4.0% =
~46% of the workload in the generic-for machinery.

### Working design (challenge it)

A per-TFOR_CALL-site cursor in `RuntimeCaches`, carried by the instruction's
C operand, which is reserved-zero today (`verify.rs`: "Two-byte forms
reserve C" - OP_TFOR_CALL | OP_CALL => c == 0):

- **Encoding**: reuse loop 4's proven biased scheme - C = 0 uncached (every
  legacy save), C >= 1 means cursor slot C-1. `finalize` assigns fresh slots
  per TFOR_CALL site; verifier mirrors the SET_GLOBAL first-use-stream
  rules; FORMAT_VERSION stays 6 (legacy saves just never use cursors).
- **Slot storage**: a new `tfor_cursor` family in `RuntimeCaches`
  (`Cell<Option<TforCursorEntry>>`). CRITICAL: do NOT add a slot-count
  field to `Bytecode` if that field would enter the snapshot encoding -
  check how `global_cache_slots` etc. are serialized and either mirror
  that safely (would it change the format?) or derive the count by
  scanning the code for OP_TFOR_CALL in `RuntimeCaches::new`. If
  mirroring changes the saved-bytecode encoding, derive instead - a
  format bump is not acceptable for this feature.
- **Entry**: `(table: ObjectPtr, index: usize)` - the storage index where
  `control` was found last step. Validation is SELF-CONTAINED:
  `state.as_object_ptr() == entry.table` AND the table's entry at
  `entry.index` is live with key == control. On validation, the next pair
  is the first live entry after `entry.index` (a forward walk that skips
  dead slots EXACTLY as `Table::next` does); store the new index. On any
  mismatch, fall back to `Table::next(&control)` and repopulate from its
  found position (Table will need a `next`-variant that also reports the
  index it found/returned).
- **Why key-at-index validation, not version**: RuntimeCaches is shared
  per (State, Bytecode) since b14fde8, so ONE site's cursor can be hit by
  concurrently active iterations - recursion like
  `function f(t) for k in pairs(t) do f(t) end end` runs nested
  iterations of the same site over the same table. `(ptr, version)` alone
  would validate while the shared cursor points at the INNER iteration's
  position, corrupting the outer's sequence - a correctness bug, not
  warmth. Key-at-index makes the cursor self-validating: a repositioned
  cursor fails the control-key compare and falls back to the hash path,
  which is always correct. `Table::version` may still be worth carrying to
  cheapen the common case (skip the key compare when version matches?) -
  NO: version does not bump on tail appends/tombstones, and the key
  compare is one Val compare against an already-loaded entry; keep the
  entry minimal unless the reviewer finds a real win.
- **Iteration-order identity**: the cursor walk must produce byte-for-byte
  the sequence today's `Table::next` produces on BOTH storage variants
  (Inline scan and Map index walk), including dead-slot skipping,
  `TableNext::End` at the tail, and `InvalidKey` behavior (a control key
  not present -> fall back, which yields InvalidKey exactly as today,
  making `instr_tfor_call_next` return false into the generic path).
  Consider whether Inline tables should bypass the cursor entirely (their
  `next` is already a tiny scan; a cursor may be pure overhead there).
- **Mutation during iteration**: reference-Lua UB territory, but dellingr
  today has DEFINED deterministic behavior via `Table::next` - the cursor
  must not change any of it. Tombstoning the control key itself, appending
  during iteration, compaction (version bump + binding shift): each must
  land in the same observable sequence as today. The existing table unit
  tests around `next` + tombstones (`table.rs` tests) are the oracle;
  extend them through the cursor path.

### Constraints (inline; sessions read nothing else)

- Read and write code only; no cargo/brokkr/test/bench commands.
- Determinism: identical iteration sequences and identical `cost_used`
  (TFOR_CALL's cost status must not change - verify what it charges).
- No HashMap/HashSet; clippy denies warnings; `unwrap_used` denied outside
  tests; `Result::ok()` banned.
- `instr_tfor_call_next` returning `false` must keep meaning "take the
  generic call path" with the stack untouched.
- Verifier: TFOR_CALL leaves the reserved-C group; nonzero C validates
  against the first-use stream + declared count (mirror loop 4's tests:
  legacy zero accepted, out-of-range/wrong-order rejected).
- ipairs (`instr_tfor_call_ipairs`) is NOT in scope - it is already
  index-stepped.
- Keep `#[hotpath::measure]` annotations; `Table::next` keeps its own.

### Deliverable

Implementation plan: entry/validation shape (argue the self-validation
design or improve it), the slot-count serialization answer (derive vs
field), Table API addition (`next_from_index`-style walk + index-reporting
`next`), verifier changes, test list (order-identity oracle across both
storage variants + tombstone/append/compaction mid-iteration + recursion
over one site + forged-save operands + exact-cost pin), bench prediction
(`pairs` moves materially; `benchmark`/`mixed` composites hold or improve;
nothing regresses; `cost_used` identical).

---

## Agreed plan (consolidated 2026-07-26; implement exactly this)

A control-key-validated cursor, NOT version-validated. Three review
verdicts baked in: (1) the same-site hazard is real (shared
`Arc<BytecodeRuntime>` means recursive invocations overwrite one site
slot; identity+version would falsely validate; the slot-key compare is
sufficient because table keys are unique) - do NOT carry or consult
`Table::version`; (2) validation must accept a TOMBSTONED control slot
(key matches, value nil): `Table::next` treats dead controls as valid
positions and only skips dead slots while finding the successor, so
requiring liveness would restore the hash probe for filter-in-place
iteration; (3) the three existing cache-count fields ARE serialized bytes
in `SavedBytecode` - adding a fourth changes format 6, so the cursor
count is DERIVED from the code (the validated C high-water mark), no new
field anywhere.

### Encoding + allocation

- `TFOR_CALL.C`: 0 = uncached (all legacy saves); 1..=255 = cursor slot
  C-1. Add `Instr::tfor_call_cached` (keep the legacy constructor),
  include C in Debug formatting.
- `assign_cache_slots`: fresh slot per TFOR_CALL site in instruction
  order; 256th+ sites stay C=0. NO `Bytecode`/`SavedBytecode` count
  field.

### Runtime storage

- `TforCursorEntry { table: ObjectPtr, index: usize }` in a
  `Cell<Option<_>>` slot; `tfor_cursor: Vec<_>` on `RuntimeCaches`,
  sized in `RuntimeCaches::new` by scanning the code for the largest
  nonzero TFOR C operand (verified first-use ordering makes the
  high-water mark the exact count). Entries are non-rooting like every
  other object-pointer cache; validation dereferences only the live
  table from the current iterator state.

### Table API (refactor, do not duplicate `next` semantics)

- One storage-specific forward-walk primitive: scan from a supplied
  index -> `Pair { index, key, value }` or `End`.
- `next_with_index(control)`: NaN rejected; nil starts at 0; otherwise
  locate the physical control slot INCLUDING a tombstoned one;
  physically absent -> `InvalidKey`; then forward-walk skipping
  nil-valued slots.
- `next_from_matching_index(index, control)`: validate the raw slot key
  equals control (regardless of value liveness), then forward-walk in
  the same storage match.
- Reimplement the existing annotated `Table::next` as a projection of
  the indexed variant so direct `next()` calls keep their behavior and
  annotation.

### VM fast path (`instr_tfor_call` / `instr_tfor_call_next`)

- Thread `inst.c()` + the frame's cache bundle through.
- Cursor consulted only after the iterator is exactly builtin
  `base_next` and the state is a table. `checked_sub(1)` + `Vec::get`;
  C=0/unavailable -> `next_with_index`.
- Pointer match -> `next_from_matching_index`; any mismatch ->
  `next_with_index`. On a pair: store (table ptr, returned index), write
  results. On End: nils as today. On InvalidKey: no result writes, no
  cursor population, return `false` so the generic path produces the
  standard "invalid key to 'next'" error with the stack untouched.
- One path for both storage variants (Inline included - it removes
  prefix rescans and one path is less semantic risk; revisit only if
  measurement shows an Inline regression).

### Verifier + snapshots

- TFOR_CALL leaves the reserved-C group (CALL stays). Track
  `next_tfor_cursor_slot`: C=0 accepted anywhere; nonzero requires
  `C - 1 == next_tfor_cursor_slot` (gaps, duplicates, reversals, and
  post-255 values rejected), then increment. The validated high-water
  mark IS the declaration - deliberately no final count-equality check
  for this family. `FORMAT_VERSION` stays 6.

### Tests

- Compiler: sequential C per site; 255-site cap; 256th at C=0.
- Table oracle (Inline AND Map): exact key/value/index sequences; nil
  start; dead prefix/middle/tail skipping; tombstoned control accepted;
  tail End; absent + NaN -> InvalidKey.
- Cursor mutation through real `pairs` bytecode: tombstone current
  control; tombstone future entries; tail append (no version bump);
  inline append + Inline->Map promotion; compaction shifting the current
  binding; tombstone-reinsertion moving a key to the back.
- Same-site recursion: one recursive function whose single pairs site
  walks `a,b,c` at depth two - the outer traversal must retain its
  `b,c` suffix after the inner overwrites the shared slot.
- Preserve/extend generic-for invalid-control tests (`false` still
  reaches builtin `next` -> "invalid key to 'next'").
- Forged saves: legacy C=0 accepted; C=1 and C=1,2 streams accepted;
  first C=2 and C=255 rejected as gaps; duplicates/reversals rejected;
  round-trip preserves nonzero C and rebuilds cold slots.
- Exact-cost pin: three-field table walked twice with an empty pairs
  body = cost 4 (one creation + three writes), cold and warm.

### Bench acceptance (orchestrator)

`pairs` moves materially; `benchmark` improves modestly; registered
`mixed` (string-focused) holds; `ipairs`, direct `next`, and every
`cost_used` identical.
