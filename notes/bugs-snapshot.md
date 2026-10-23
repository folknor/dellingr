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
so it needs a `FORMAT_VERSION` bump. `FORMAT_VERSION` 12 is still unreleased, so
doing it before the next release avoids a second bump.

## SNP-013 - `Table::get_array` still allocates a whole sequence infallibly

Lateral from the third resolution wave, which made array growth fallible so a
forged logical length errors instead of aborting. `Table::get_array`, used by
`table.sort`'s raw path to copy the sequence out, still allocates the whole
border-length vector with an infallible allocation. A forged sparse table with
a huge border can reach that abort through `table.sort` even though appends and
out-of-window writes no longer can. `table_sort` does call
`checked_table_sort_len` before `get_array`, but that cap is `i32::MAX`
elements, so a forged border near it still asks for an infallible allocation of
up to about 32 GiB. Make the copy fallible like the growth paths.
