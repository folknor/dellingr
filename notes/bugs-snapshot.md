# Snapshot findings: save, load, verify

Filed from the 2026-11-04 defect hunt (scope `snapshot`, with one entry also
raised by `vm-heap`). Hunt reports are work documents and may be wrong; verify
before fixing.

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

## SNAP-002 - Environment deltas are not deltas: any slot change in a library table re-imposes every field's saved value

Scope: `snapshot`. Hunter's label: medium.

Claims broken: README, "Changes you make to the standard library tables ... are
persisted as a delta against the pristine environment and replayed on load";
`docs/save-state.md`, "Standard-library tables as deltas against the pristine
environment" and "The load setup callback configures the rebuilt environment".

`encode_env_deltas` emits `hash_slots` whenever a table's physical slots differ
from the baseline's, and then encodes every physical slot with its value, not
only the changed ones (the comment "Values are part of this comparison
deliberately" confirms it). On load, the `hash_slots` branch of
`materialize_payload` treats every saved key as authoritative and drops setup's
live value for any key the save mentions (`saved_keys` excludes it from the
extras). Whether a setup override of a library field survives a load thus
depends on whether the script changed some other field of the same table:

- Save A: the script never touched `math`, so there is no delta. Load setup
  replaces `math.floor` with a host function; the override survives.
- Save B: the script did `math.myconst = 42`, so `hash_slots` holds
  `(floor, Fn("math.floor"))`. The same setup override is silently reverted to
  the stock function.

The `order` section has the same issue: it lists every live key, unchanged ones
included. Consequences: a host that changes its setup between game versions
(sandboxing a library function, swapping `math.random`) has the change reverted
only in saves whose scripts touched that table. Compatibility is equally
inconsistent: if a later build drops a library function, an old save with a
`math` delta fails with `UnknownFunction("math.bar")` (from `hash_slots`) or
`OrderKeyAbsent` (from `order`), while a save without one loads. The doc
promises only that adding a field is safe, but the failure depends on unrelated
script edits no host can predict.

Structural fix the hunter gave, pick one model: (a) treat each environment table
like `_G`, saved whole and authoritative for every key it holds, with setup
contributing only keys the save never mentions; document that and drop the
delete/upsert/order machinery, whose result `hash_slots` overwrites anyway in
every save where it matters. Or (b) make the slot history value-free for
unchanged keys: record tombstone, vacancy and position per slot, but carry a
value only for keys in `upserts`, so unchanged keys keep setup's value. The
hunter prefers (a) as simpler and consistent with the format-13 `_G` design.

## SNAP-008 - Lateral: PathFinder can report a path that is not the edge the encoder followed

Scope: `snapshot`.

`PathFinder` (unregistered-function diagnostics) re-expands `_G` and other
environment objects with fresh visited sets, while the encoder treats `_G` as a
token. The reported path can run through `_G` to another global holding the
same function. The path is valid, but not the edge the encoder actually
followed, which the comment implies it is.
