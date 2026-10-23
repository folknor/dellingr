1. An entry is removed entirely when completely resolved. No historical record
   stays here.
2. Stable IDs never change and are never reused; removal leaves a gap.
3. An entry adjudicated against, verified incorrect, or whose outcome is that no
   action is taken owes comments at the code sites it names - and, where the
   claim touches a documented contract, the relevant `reference/` or `docs/`
   page - before the entry is removed, so the finding is not hunted again.
4. Once all findings are resolved, the file gets deleted.
5. Finding IDs are never written into the code or other documents. They are
   stable only until this document is drained; the next hunt writes new ones,
   and they are never deduplicated through git history. Carry the context
   inline instead.

# Snapshot defects: save, load, identity, replay

Filed from the 2026-10-23 defect hunt at a22d9c0 (scopes `snapshot` and
`tables`). The hunters read the code statically and built or ran nothing, so
every repro here is traced by hand, not executed. The `snapshot` hunter found
rng.rs clean: the SplitMix64 step, the 53-bit `random_f64` scale and the
rejection threshold are correct, and `math.random` validates and charges before
drawing.

## SNP-003 - Rendered identities of environment objects are dropped though the objects survive the load

From the `snapshot` hunter.

**Claims broken**

- docs/determinism.md "Object identity in output": `tostring` and `%p`
  identities are "stable across a snapshot round trip".
- docs/save-state.md "What is saved": "State-local object identities that
  `tostring` and `%p` have rendered".
- The save-side comment in `SaveBuilder::finish` justifies the filter with "A
  heap value unreachable from saved state cannot survive the load", which is
  false for environment objects.

**Mechanism.** `SaveBuilder::finish` keeps a `format_pointer_ids` entry only if
`saved_reachable_pointer_value` resolves it and the value is in
`reachable_pointer_values` (the `Fn` case is exempt). `reachable_pointer_values`
is filled only by the value walk rooted at globals and environment deltas, so an
`EnvObj` is in it only if some saved value points at it.

Environment objects are rebuilt by `open_libs` on every standard load, so they
always survive, and some are reachable by scripts without any global:

- The shared string metatable (`string.metatable` token). `getmetatable("")`
  returns it, and it has no `__metatable` protection (`install_string_metatable`
  in src/vm.rs; `raw_metafield` in basic.rs). No global or table refers to it, so
  the walk never reaches it.
- The `string` library table once the `string` global is shadowed or deleted. It
  is still reachable as `getmetatable("").__index`.

**Repro sketch.** `print(getmetatable(""))` prints `table: 0x1`. Save, load, and
run `print(getmetatable(""))` again. The loaded State hands out a fresh id
(`next_format_pointer_id` was restored, but the entry was not), while the
continuous run prints `0x1`. That is a replay-visible divergence between a
loaded State and the run it came from. In the live State the entry is never
pruned, because the string metatable is a GC root (`mark_gc_roots`, vm.rs), so
the save is the only place the identity is lost.

The same gap covers strings reachable only through an environment object the
walk did not expand: for example the keys of the `string` table once `string` is
deleted, enumerated through `pairs(getmetatable("").__index)` and rendered with
`%p`. The identity-string loop covers meta names and literals of saved chunks,
but not environment-table keys or values.

**Direction.** The set that survives the load is everything `open_libs` rebuilds
plus the saved graph, not the saved graph alone. Seed the reachability walk with
every `env_tokens` object, expanded the same way as the existing `Discard`
expansion. Simpler still: keep every `EnvObj` and every string or function
reachable from an environment object. Tests should pin `getmetatable("")` and a
shadowed `string` table across a round trip.

## SNP-005 - A forged logical array length turns the next append into a host abort or panic

From the `snapshot` hunter.

**Claims broken**

- docs/limits.md "State-local growth": for the logical array length cap,
  "growing past it panics, and a save beyond it is rejected on load".
- The `MAX_ARRAY_LEN` doc in table.rs: "Snapshot loading rejects a larger length
  rather than panicking."
- docs/save-state.md treats saves as user-editable input.

**Mechanism.** `validate_references` (verify.rs) bounds a saved table's
`array_len` only by `MAX_ARRAY_LEN` (2^53 - 2), independently of `array_slots`.
`rebuild_from_snapshot` then builds a table with `vlen = array_len` and a window
holding only the stored span. Live execution reaches a large `vlen` only by
paying for that many appends, and paying in storage unless a FIFO drained them.
A save of a few dozen bytes reaches it for free:

- `array_len = 2^52`, `array_slots = 1` (key 1 live). Any write to key
  `2^52 + 1` takes the append path. `prepare_append` sees
  `base + array.len() < vlen` and calls
  `self.array.resize(self.vlen - self.base, Val::Nil)`: about 2^52 x 16 bytes.
  The allocator fails and Rust aborts the process (`handle_alloc_error`). The
  host gets no error.
- `array_len = MAX_ARRAY_LEN` with an empty window. The append reaches
  `grow_vlen`, whose `assert!` panics.

The forged save can carry the triggering bytecode itself, and the cost budget
does not charge the resize. A save the load verifier accepts can therefore kill
the host on the first script write.

**Decided (user, 2026-10-23):** make table growth fallible, as described under
"What remains".

**Both proposed directions were refused** in the first resolution wave, with
comments left in src/vm/save_state/verify.rs and src/vm/table.rs and a test
pinning the constraint:

- Bounding `array_len` by the stored span plus a slack rejects valid saves: a
  live table can end in a long structural nil tail, so `array_len` legitimately
  exceeds the stored span by any amount.
- Resetting the window on a large gap discards live values.

**What remains.** The gap fill has to become fallible: table growth returns an
error instead of aborting or asserting, and that error propagates through table
writes. The fixer named `array_insert` (to return `Result`) and its caller
`table_ops.rs::table_insert_at` as the first sites. `write_outside_window` can
allocate an equally large gap and needs the same treatment; fixing only the
append path leaves it exposed. docs/limits.md's "growing past it panics" and
the `MAX_ARRAY_LEN` doc then need to match whatever error the growth path
returns.

Lateral from the same wave: the save-golden test's failure diagnostic equates a
traversal reordering with an unreadable save, which misdirects whoever reads
the failure.

## SNP-007 - Identities rendered during load setup are silently replaced

From the `snapshot` hunter.

**Claims broken**

- The doc of `State::format_pointer_id` (table_ops.rs): "a deterministic,
  state-local identity".
- docs/determinism.md: "Identities are never reused." The verifier enforces
  `id < next_format_pointer_id` precisely so that "the next fresh value [cannot]
  render the same identity as a live one".

**Mechanism.** `load_state` runs `setup` on a fresh State, and setup can run Lua
and render values. docs/save-state.md says a sealed setup "must replay the
initialization the saving State performed", so this is expected use.
`materialize_payload` then overwrites both `state.next_format_pointer_id` and
`state.format_pointer_ids` with the saved ones.

Take a value that outlives setup and was rendered during it: an anchor created
there ("Anchors created there survive"), a setup-only global, or a registered
host function. It rendered as some id `k` during setup. After the load it either
renders as a different id (its entry is gone), or shares `k` with a saved value
that was assigned `k` in the saving State. Both ids came from the same counter
start, so the second case is the norm, not a corner case.

**Direction.** Merge instead of replacing: keep setup entries whose values
survive, re-key them above the saved counter, or reject or advance on collision.
The simplest consistent rule is that setup must not render identities, since it
is initialization, and that rule could be enforced. As written, the code keeps
neither promise.

## SNP-008 - A cross-folding load fails outright when two folded functions were rendered or used as keys

From the `snapshot` hunter.

**Claim broken.** docs/save-state.md "Rust functions": "a folded pair loads back
as the smaller id's function". The sentence is unconditional.

**Mechanism.** The materialize pass repeats duplicate checks on runtime values:
table keys, environment delta keys, and `format_pointer_ids` through
`insert_runtime_key`. Take a save from a build where `f` and `g` are distinct,
such as a debug build (the golden fixture is regenerated in debug because
release folds), and load it into a build where they fold. The load fails with
`Duplicate { FormatPointerValue | TableKey, Materialized }` as soon as both
functions were printed, or both are keys of one table. In that case the pair does
not "load back" at all. The code comments know this ("its failures are the ones
that catch those cases"); the user-facing doc does not say it.

**Direction.** Either state in docs/save-state.md that such saves are rejected,
or resolve deterministically: for pointer ids, keep the smaller id's entry. Table
keys genuinely collide, so rejection is defensible there.

## SNP-009 - Environment deltas carry a second, non-canonical array encoding

From the `snapshot` hunter, raised as a structural note outside the defect bar.

`encode_env_deltas` calls `table.virtual_array()`, which allocates
`vec![Nil; vlen]` and encodes every slot. A library table used as a FIFO
(`table[#table+1] = x; table[head] = nil`) therefore makes every save O(total
pushes ever), in both memory and bytes, while ordinary tables save O(live span).
docs/save-state.md documents this, so it is not a defect, but it exists only
because env deltas predate the canonical `(vlen, span)` form. Encoding the env
array in the same `snapshot_entries` form would remove `virtual_array`, the
`ArrayDisagreesWithUpserts` and `OrderDisagreesWithArray` cross-checks, and the
materialize-side merge of setup slots beyond the saved range. The hunter
considers it worth a rewrite of the delta array section. It changes the format,
so it needs a `FORMAT_VERSION` bump.

## SNP-011 - Small load-path observations: setup's stack, error wording, misleading delta path

From the `snapshot` hunter, raised outside the defect bar.

- **Setup's stack is discarded without a word.** `reset_in_flight` at the end of
  `materialize_payload` drops anything setup left on the value stack. That is
  harmless, but nothing documents that setup's stack is not preserved. A host
  that pushes a value in setup and expects it after load gets an empty stack and
  no error.
- **Error wording.** `EnvironmentCorruption::MetatableNotTable(Materialized)`
  renders as "saved environment metatable is not a table". Every other
  `Materialized` variant says "materialized ...", and this one reads like the
  `Saved` form. Cosmetic (error.rs, `impl Display for EnvironmentCorruption`).
- **Misleading unregistered-function path from environment deltas.** When an
  unregistered function is reached only through an environment delta (for
  example `string.f = hostfn` with the `string` global deleted), `PathFinder` is
  rooted with `global = token`. The report then reads `global string ...` even
  though no global of that name holds it. Diagnostic only.

## SNP-012 - Follow-ups to saving hash-slot history (format 12)

Laterals from the second resolution wave's reconciliation and review, after
dead hash slots became part of a table's saved shape.

- **Env deltas grow on any value change.** An environment table's delta now
  carries its whole `hash_slots` list whenever any value differs from the
  baseline, not only on a structural change, because `snapshot_hash_slots`
  includes values. Correct and byte-stable, but saves of a State that only
  reassigned a library field carry the full slot list of that table. Emitting
  slots only when the key layout differs from the baseline, with values in the
  existing upsert list, would keep deltas proportional to the change.
- **Misleading location for a corrupt env slot.** `validate_table_shape` is
  reused for env `hash_slots` with `object_index = delta_index`, so a bad slot
  is reported as `TableEntries { object: <delta index> }`, naming an object that
  is not the one at fault. The input is still rejected.
- **Dead env-table keys are invisible to the unsaveable-function path.**
  `PathFinder` walks env tables with `entries()`, which yields live entries
  only. An unregistered Rust function held only as a dead env-table key (a
  tombstone, now kept alive and saved) still fails the save, but the report has
  no "reachable from" path.
- **Unaudited key trust.** Env hash-slot restore in `materialize_payload` calls
  `TableKey::from_prevalidated` on decoded saved keys, relying on verify.rs
  having validated them. Confirm the verifier covers every slot that reaches
  that call.
