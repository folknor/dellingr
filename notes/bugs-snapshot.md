# Snapshot findings: save, load, verify

Filed from the 2026-11-04 defect hunt (scope `snapshot`). Hunt reports are work
documents and may be wrong; verify before acting.

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

## SNAP-002 - Standard-library tables are saved whole once touched, not as deltas

Scope: `snapshot`. Awaiting a decision by the project owner. Hunter's label:
medium.

### What the documents claim

README: "Changes you make to the standard library tables ... are persisted as a
delta against the pristine environment and replayed on load".
`docs/save-state.md`: "Standard-library tables as deltas against the pristine
environment" and "The load setup callback configures the rebuilt environment".

### What the code does

`encode_env_deltas` emits `hash_slots` for a library table whenever its physical
slots differ from the pristine baseline's, and then encodes every physical slot
of that table with its value, not only the changed ones. A code comment says the
values are part of the comparison deliberately. On load, the `hash_slots` branch
of `materialize_payload` treats every saved key as authoritative: `saved_keys`
excludes it from the setup-provided extras, so setup's live value for any key
the save mentions is dropped. The `order` section behaves the same way: it lists
every live key, unchanged ones included.

So whether a load setup override of a library field survives depends on whether
the script changed some other field of the same table:

- Save A: the script never touched `math`, so there is no delta. Load setup
  replaces `math.floor` with a host function; the override survives.
- Save B: the script did `math.myconst = 42`, so `hash_slots` holds
  `(floor, Fn("math.floor"))`. The same setup override is silently replaced by
  the stock function.

Version compatibility is uneven in the same way. If a later build removes a
library function, an old save with a `math` delta fails with
`UnknownFunction("math.bar")` (from `hash_slots`) or `OrderKeyAbsent` (from
`order`), while an old save without one loads. `docs/save-state.md` promises
that adding a field is safe; the outcome of removing one depends on unrelated
script edits.

### Context

- `_G` itself has been saved whole, and authoritative for every key it holds,
  since snapshot format 13. The current format is 14 (`FORMAT_VERSION` in
  `src/snapshot_format.rs`).
- A fix wave made the environment diff compare numbers by bits, so a signed-zero
  change to a library field is recorded; that does not change the shape
  described here.

### Models raised so far (not exhaustive)

- Treat each library table like `_G`: saved whole and authoritative for every
  key it holds, with setup contributing only keys the save never mentions. The
  delete, upsert and order machinery would no longer be needed, since
  `hash_slots` already overrides it whenever it is emitted. The documents would
  describe that model instead of a delta.
- Make the slot history value-free for unchanged keys: record tombstone,
  vacancy and position for each slot, but carry a value only for keys the script
  changed, so untouched keys keep setup's value.

Considerations noted during triage, not verified against the code: under the
first model, a setup override of a library function would not take effect for
any table the script has touched; either model appears to change what is
written, which would mean a format version bump and regenerated save goldens.
