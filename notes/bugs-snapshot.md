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

## SNAP-001 - A tampered save can price every charge at zero, and the documented mitigation no longer holds

Scope: `snapshot`. Hunter's label: medium.

Claim broken: README "Save state": "Save files are user-editable input. Loading
restores the saved cost counters, so hosts that load user-editable saves must
call `set_cost_budget` after a successful load." `docs/save-state.md` gives the
same advice ("Hosts that saved without a budget must call `set_cost_budget`
afterwards").

Since format 14 the payload also carries the costing, and `load_state` builds
the State under it (`State::builder().costing(costing)`; "a load takes it from
the save, not from the host"). `read_costing` accepts any tag-0 weight list via
`CostModel::from_weights` without validation (fine for host-built models). An
edited save can therefore:

- set every weight to 0: `set_cost_budget` succeeds, but no script charge ever
  drains the budget, so the documented mitigation silently does nothing;
- choose `Costing::Off`: `set_cost_budget` is refused with `CostingOff`, so the
  host learns of it only if it checks that result.

The budget is the game-design instrument, which makes it the first thing a
cheating player would edit. The README describes the older threat model
(counters only).

Fix options the hunter gave: at minimum, tell hosts in README and save-state.md
to compare `State::costing()` with their expected costing after loading
untrusted bytes. Better: let `load_state` take an expected `Costing` (or a
predicate) and reject a mismatch as a `SnapshotError`, so the saved costing is a
consistency check and is never trusted blindly.

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

## SNAP-003 - format_pointer_ids are written in map insertion order, so a loaded State and its continuous run can stop saving identical bytes

Scope: `snapshot`. Hunter's label: low-medium. Replay-visible only in save
bytes; a fix needs a golden regeneration.

Claims broken: `docs/save-state.md`, Byte stability: "A loaded State and the
continuous run it came from keep saving identical bytes". Load section: "A setup
that replays the saving State's initialization renders each such value with the
id its counterpart had, so the loaded State continues as the saving one would."

`SaveBuilder::walk` iterates `state.format_pointer_ids` (an `IndexMap` in render
order) and writes entries in that order. After a load the map order is saved
entries first, then setup-kept entries, then re-keyed ones
(`materialize_payload` plus `merge_setup_pointer_ids`). The continuous run's
order is pure render order. Pruning uses `retain`, which keeps order, so nothing
re-canonicalises it.

Scenario: setup renders an anchored table `A` (or `%p` of a string held only by
an anchor) during init and gets id 1. Later a script renders global table `B`,
id 2. The save drops `A` (unreachable) and keeps `B:2`. On load setup renders
`A` again (id 1, not among the saved ids, so kept). Loaded map: `[B:2, A:1]`;
continuous map: `[A:1, B:2]`. Both runs then store `A` in a global and save: the
payloads list the two records in different orders though the logical state and
every id are identical.

Fix: write `format_pointer_ids` sorted by id (ids are unique and verified below
`next_format_pointer_id`). That is canonical, does not change the format, and
makes the loaded map's order irrelevant.

## SNAP-004 - Duplicate pins with equal content are silently merged, not rejected

Scope: `snapshot`. Forged input only; an engine save never produces it.

Claim broken: `docs/save-state.md`, Load: "Duplicate keys in the saved global
table and duplicate pins are rejected, not silently replaced".

`validate_references` checks pin uniqueness by string arena id
(`insert_unique(&mut pins, *id, ...)`), not by content, and the decoder does not
reject equal-content entries in the string arena. Pins `[Str(0), Str(1)]` with
`strings[0] == strings[1]` pass verification, and `GlobalEnv::restore` merges
them silently ("Distinct saved ids can intern to one string; keep the first.").
Global table keys, by contrast, are checked by content (`SavedVal::table_key`
resolves to bytes). Fix: check pins by content as `table_key` does, or reword
the doc.

## SNAP-005 - Snapshot diagnostics that name something other than what happened

Scope: `snapshot`.

- A hash-slot restore failure in an environment delta is reported as
  `TableRestoreStep::RestoreEnvironmentOrder` ("saved environment order is
  invalid"; `materialize_payload`, `hash_slots` branch). No hash-slots step
  exists. `error.rs` says every reason is an enum a host can match on, so this
  one misleads.
- `SavedEnvDelta::encode` writes `hash_slots` under
  `SaveCollection::EnvironmentUpserts`, so a `TooLarge` there names the wrong
  collection.
- In `encode_globals`, a global whose key is not a UTF-8 string (array part
  `_G[1] = f`, non-UTF-8 names now that `State::set_table` allows them, table
  keys) is encoded through `encode_environment_root(.., "_G")`. An unregistered
  function under it is reported as `environment _G...`: the path neither names
  the key nor says it is a global.

## SNAP-006 - Latent: the environment diff compares values with Lua ==, losing a signed-zero change

Scope: `snapshot`. Nothing is lost today.

`encode_env_deltas` decides "unchanged" with `base_value == value` and
`slots == baseline.hash_slots`, both `Val::eq`, which is f64 `==`. A library
field changed from `0.0` to `-0.0` (or back) would count as unchanged and lose
its sign on load. The code comment covers the NaN half of this, not the
signed-zero half. No current library baseline holds a zero (`math.pi`,
`math.huge`), but a future constant would break "`-0.0` is exact". Fix: compare
numbers by bits, as `GlobalEnv`'s mirror check already does.

## SNAP-007 - Lateral: Table::rebuild_from_snapshot is not atomic on error

Raised by both `snapshot` and `vm-heap`.

`rebuild_from_snapshot` replaces `*self` with an empty shell (version reset to
0, metatable cleared) before validating array keys and hash keys. Every caller
is a load path that discards the State on failure, so nothing observes the
half-rebuilt table; it is safe only because of that. The `vm-heap` hunter adds
that the version comment ("must move past every value an inline cache could
have captured") holds only on the success path. Validating before replacing
removes the dependency on the caller.

## SNAP-008 - Lateral: PathFinder can report a path that is not the edge the encoder followed

Scope: `snapshot`.

`PathFinder` (unregistered-function diagnostics) re-expands `_G` and other
environment objects with fresh visited sets, while the encoder treats `_G` as a
token. The reported path can run through `_G` to another global holding the
same function. The path is valid, but not the edge the encoder actually
followed, which the comment implies it is.
