# Hunt: snapshot (save_state.rs, save_state/verify.rs, save_state/error.rs, snapshot_format.rs)

Read-only hunt. Each finding names the claim it breaks. Ordered by impact.

## 1. A tampered save can price every charge at zero; the README's mitigation no longer holds (medium, contract/doc)

Claim broken: README "Save state": "Save files are user-editable input. Loading
restores the saved cost counters, so hosts that load user-editable saves must
call `set_cost_budget` after a successful load." `docs/save-state.md` repeats
the same advice ("Hosts that saved without a budget must call
`set_cost_budget` afterwards").

Since format 14 the payload also carries the costing, and `load_state` builds
the State under it (`State::builder().costing(costing)`; "a load takes it from
the save, not from the host"). `read_costing` accepts any tag-0 weight list
(`CostModel::from_weights`, no validation, which is fine for host-built
models), so an edited save can:

- set every weight to 0. `set_cost_budget` then succeeds, but no script charge
  ever drains the budget. The documented mitigation silently does nothing.
- choose `Costing::Off`. `set_cost_budget` is refused with `CostingOff`, so the
  host learns about it only if it checks that result.

The budget is the game-design instrument, so this is the part of a save a
cheating player would edit first. The README describes the old threat model:
counters only.

Fix options: tell hosts to compare `State::costing()` with their expected
costing after loading untrusted bytes (minimum: a doc fix in README and
save-state.md). Better: let `load_state` take an expected `Costing` (or a
predicate) and reject a mismatch as a `SnapshotError`, so a host cannot forget
to check. The saved costing then serves as a consistency check and is never
blindly trusted.

## 2. Environment deltas are not deltas: any hash change in a library table re-imposes every field's saved value (medium, contract)

Claims broken: README "Changes you make to the standard library tables ...
are persisted as a delta against the pristine environment and replayed on
load". `docs/save-state.md` "Standard-library tables as deltas against the
pristine environment" and "The load setup callback configures the rebuilt
environment".

`encode_env_deltas` emits `hash_slots` whenever the table's physical slots
differ from the baseline's. It then encodes every physical slot *with its value*,
not only the changed ones (the comment "Values are part of this comparison
deliberately" confirms this). On load the `hash_slots` branch of
`materialize_payload` treats every saved key as authoritative and drops
setup's live value for any key the save mentions (`saved_keys` excludes it from
the extras). Whether a setup override of a library field survives a load
therefore depends on whether the script changed some *other* field of the same
table:

- Save A: script never touched `math`, so no delta. Load setup replaces
  `math.floor` with a host function, and the override survives.
- Save B: script did `math.myconst = 42`, so `hash_slots` holds
  `(floor, Fn("math.floor"))`. The same setup override is silently reverted to
  the stock function.

The `order` section has the same issue: it lists every live key, unchanged ones
included. The consequences:

- A host that changes its setup between game versions (sandboxing a library
  function, swapping `math.random`) gets its change reverted only in saves
  whose scripts touched that table.
- Compatibility is equally inconsistent. If a later build drops a library
  function, an old save with a `math` delta fails with
  `UnknownFunction("math.bar")` (from `hash_slots`) or `OrderKeyAbsent` (from
  `order`), while a save without one loads. The doc promises only that
  *adding* a field is safe, but the failure depends on unrelated script edits,
  which no host can predict.

Structural fix: pick one model and apply it consistently. Either
(a) treat each environment table like `_G`: saved whole and authoritative for
every key it holds, setup contributing only keys the save never mentions.
Document it that way, and drop the delete/upsert/order machinery, whose result
`hash_slots` overwrites anyway in every save where it matters. Or (b) make the
slot history value-free for unchanged keys: record tombstone, vacancy and
position for each slot, but carry a value only for keys in `upserts`, so
unchanged keys keep setup's value. (a) is simpler and matches the `_G` design
from format 13. Today's hybrid is what produces the surprise.

## 3. `format_pointer_ids` are written in map insertion order, so a loaded State and its continuous run can stop saving identical bytes (low-medium, byte stability)

Claim broken: `docs/save-state.md` Byte stability: "A loaded State and the
continuous run it came from keep saving identical bytes". Load section: "A
setup that replays the saving State's initialization renders each such value
with the id its counterpart had, so the loaded State continues as the saving
one would."

`SaveBuilder::walk` iterates `state.format_pointer_ids` (an `IndexMap`, render
order) and writes the entries in that order. After a load, the map order is:
saved entries first, then setup-kept entries, then re-keyed ones
(`materialize_payload` + `merge_setup_pointer_ids`). The continuous run's order
is pure render order. Pruning uses `retain`, which keeps order, so nothing
re-canonicalizes it.

Scenario: setup renders an anchored table `A` (or `%p` of a string held only
by an anchor) during init and gets id 1. Later a script renders global table
`B` and gets id 2. The save drops `A` (unreachable) and keeps `B:2`. On load,
setup renders `A` again (id 1, not in the saved ids, so it is kept). Loaded
map: `[B:2, A:1]`. Continuous map: `[A:1, B:2]`. Both runs then store `A` in
a global and save. The payloads list the two records in different orders,
although the logical state and every id are the same.

Fix: write `format_pointer_ids` sorted by id (ids are unique and verified
below `next_format_pointer_id`). That gives a canonical order, and the format
does not change. The loaded map order then no longer matters. Replay-visible
only in save bytes. Call it out with a golden regeneration.

## 4. Duplicate pins with equal content are silently merged, not rejected (low, contract)

Claim broken: `docs/save-state.md` Load: "Duplicate keys in the saved global
table and duplicate pins are rejected, not silently replaced".

`validate_references` checks pin uniqueness by *string arena id*
(`insert_unique(&mut pins, *id, ...)`), not by content. The decoder does not
reject equal-content entries in the string arena. So pins `[Str(0), Str(1)]`
with `strings[0] == strings[1]` pass verification, and `GlobalEnv::restore`
merges them silently ("Distinct saved ids can intern to one string; keep the
first."). Global table keys, by contrast, are checked by content
(`SavedVal::table_key` resolves to bytes). Either check pins by content (as
`table_key` does) or reword the doc. An engine save never produces this, so
only forged input is affected.

## 5. Diagnostics that say something other than what happened (low)

- A hash-slot restore failure in an environment delta is reported as
  `TableRestoreStep::RestoreEnvironmentOrder` ("saved environment order is
  invalid"; `materialize_payload`, hash_slots branch). No hash-slots step
  exists. `error.rs` says every reason is an enum a host can match on, so
  this one misleads.
- `SavedEnvDelta::encode` writes `hash_slots` under
  `SaveCollection::EnvironmentUpserts`. A `TooLarge` there names the wrong
  collection.
- In `encode_globals`, a global whose key is not a UTF-8 string (array part
  `_G[1] = f`, non-UTF-8 names now that `State::set_table` allows them, table
  keys) is encoded through `encode_environment_root(.., "_G")`. An unregistered
  function under it is reported as `environment _G...`: the path neither names
  the key nor says it is a global.

## 6. Latent: the environment diff compares values with Lua `==` (low, latent)

`encode_env_deltas` decides "unchanged" with `base_value == value` and
`slots == baseline.hash_slots`, both `Val::eq`, which is f64 `==`. A library
field changed from `0.0` to `-0.0` (or back) would count as unchanged and the
sign would be lost on load. The comment notes the NaN half of this, not the
signed-zero half. No current library baseline holds a zero (`math.pi`,
`math.huge`), so nothing is lost today. A future constant would break
"`-0.0` is exact". Compare numbers by bits, as `GlobalEnv`'s mirror check
already does.

## Checked and found sound

- Decoder bounds, reservation capping, tag strictness, trailing bytes,
  costing/meter consistency (`CostState::from_snapshot`).
- Nested-chunk graph (indegree/Kahn, depth, cycles), and closure/upvalue counts
  checked before `build_bytecode_tree`.
- Arena and globals table shapes: canonical span, append key, marker and
  vacancy handling, runtime re-check of key uniqueness after token resolution.
- No GC during materialization. The final collect runs after
  `reset_in_flight`, and liveness for the identity merge is read straight
  after it.
- Builtin mirror and global caches after `GlobalEnv::restore`: `as_table`
  moves the watched epoch and `rebuild_from_snapshot` bumps the version.
- Forged gmatch state: the native iterators re-validate their state table on
  every call, so a misplaced `Fn("string.gmatch.iter")` cannot do more than
  raise an error.
- RNG: SplitMix64 accepts any state word.

## Lateral

- `src/snapshot_format.rs` uses `#[cfg_attr(not(feature = "snapshot"),
  allow(dead_code))]`, a lint suppression the repo rules forbid. Gating the
  constant (or its non-snapshot users) would remove the need for it.
- `PathFinder` (unregistered-function diagnostics) re-expands `_G` and other
  environment objects with fresh visited sets, while the encoder treats `_G`
  as a token. The reported path can therefore run through `_G` to another
  global holding the same function. The path is still valid, but it is not
  the edge the encoder actually followed, which the comment implies it is.
- A failing `rebuild_from_snapshot` leaves the table half-rebuilt (it replaces
  `*self` before validating array keys). This is safe only because every
  caller is inside an all-or-nothing load. Validating before replacing would
  remove that dependency on the caller.
