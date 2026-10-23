# Save state

Details of the `snapshot` feature beyond the README. The format and design are
in the module doc of `src/vm/save_state.rs`.

## What is saved

- User globals, with the reachable tables, closures, upvalues and strings.
- The RNG stream and the cost counters.
- State-local object identities that `tostring` and `%p` have rendered,
  including those of the environment objects `open_libs` rebuilds on load.
- Standard-library tables as deltas against the pristine environment.

Not saved: call stack, program counter, open upvalues, coroutines, anchors,
callbacks, host user-data. Saving is valid only while the VM is quiescent.
The load setup callback configures the rebuilt environment and function
registrations. Values it leaves on the value stack are discarded before load
returns; the loaded State starts with an empty stack.

## Environment objects and deltas

- `math`, `string`, `table`, the metatable of `_G` and the shared string
  metatable are rebuilt by `open_libs` on load and referenced by token. The
  string metatable's token is `string.metatable`.
- The global table, which `_G` names, exists in every State (a raw one
  included). A reference to it is saved as the token `_G` and resolves to the
  loaded State's global table; its contents are saved whole (see Globals
  below), not as a delta.
- The object-to-token map is refreshed at each successful `open_libs`, not
  read from the live builtin slots. Shadowing a builtin (`math = {}`) cannot
  misclassify the user's table as an environment object.
- Per environment table, a delta records:
  - keys deleted from the baseline;
  - keys added or changed;
  - the key order, only when replaying the first two would not reproduce it;
  - the logical array length and the canonical span from its first live
    array key to its last, including interior nil slots, just as for ordinary
    tables (see Byte stability);
  - hash-slot history when it differs from the baseline, including dead keys,
    vacant positions and the map storage marker;
  - the metatable: unchanged, cleared, or replaced.
- The array shape is saved because it decides where later integer writes
  land. For a saved array delta, that saved shape is authoritative. Setup-only
  live keys beyond its logical length survive as ordinary writes, including
  an append key; setup-only nil tails are not merged into the saved shape.
- Adding a field to a library table in a later build does not corrupt old
  saves.
## Globals

- The global table is saved whole, like any table: its array part and every
  hash slot in physical order, nil tombstones included, so the loaded table
  iterates and compacts exactly as the saving one would. An unshadowed builtin
  encodes as a token; a shadow is preserved as the user's value. Keys of any
  type are kept, non-UTF-8 strings included.
- The save is authoritative for every key it holds, tombstones included, and
  for the array part. Load setup contributes only string-keyed globals the
  save mentions nowhere (neither as a slot nor as a pin); they follow the
  saved slots, in setup order. Setup's other keys (numbers, booleans, tables,
  functions) are discarded.

## Sealed globals

- `State::seal_globals` ends global initialization (see
  [limits](limits.md)). Until then the global table keeps every tombstone.
  Sealing records the pins: every string key with a slot at that moment,
  which is every name initialization wrote, including ones it set and then
  cleared. After sealing, tombstones compact as in any table.
- The save records whether the State was sealed, and its pins. The loaded
  State takes that phase: a call to `seal_globals` inside the load setup is
  deferred to it. A pinned name absent from the saved table was deleted and
  compacted away, so a load keeps it deleted even when setup reinstalls it.
- A sealed load pins the saved pins followed by every other string key the
  setup gave a slot, so a deletion inherited from a save survives a setup that
  no longer installs the name, and the resave after it. With a setup that
  replays the saving State's initialization, the loaded pins equal the saved
  ones, the loaded State compacts exactly when the saving one would, and later
  saves of both stay byte-identical. An unsealed save has no pins.

## Numbers

- Numbers are written by bit pattern. `-0.0` is exact.
- Every NaN is written as one canonical quiet NaN, so host differences in NaN
  sign or payload do not change snapshot bytes. This also applies to NaN
  literals in bytecode. No script can observe the difference.

## Rust functions

- A reachable `RustFunc` is saved by its registered stable id. An unregistered
  one fails the save.
- The standard library registers itself under dotted paths (`math.sin`,
  `ipairs.iter`, ...) during `open_libs`, including iterator callbacks returned
  by library functions. `open_libs` rejects conflicting registrations before
  installing anything, leaving the stack, globals, heap and registry unchanged.
- A save records the rendered identity (`tostring`, `%p`) of every registered
  Rust function that has been printed, including one only the host still
  holds. Passing it back in after a load prints the same `function: 0x...`.
- On load, a recorded function that setup no longer registers is skipped.
- Rust function identity is by code address. When several ids share one
  address (intentional aliases, or identical-code folding in a release/LTO
  build), the lexicographically smallest id is written. A build that folds
  functions can therefore write different `Fn` ids than one that does not, so
  snapshot bytes are only guaranteed identical across the same build.
- Loading a save into a build that folds two functions the saving build kept
  apart resolves both ids to the one folded function. If both were rendered,
  it keeps the smaller of their two rendered identities and retires the other.
  If both are keys of the same table, dead keys and standard-library table
  deltas included, the keys collide and the load is rejected.

## Load

- Setup runs inside `load_state`; its closure returns `Result<(), E>`, surfaced
  as `LoadError::Setup(E)`. Anchors created there survive.
- Setup may render identities (`tostring`, `%p`). After the load, a value the
  save records renders with its saved identity, whatever setup printed for it.
  Any other value setup rendered that survives the load (an anchor or a value
  only it reaches, a setup-only global, a newly registered function) keeps its
  setup identity unless a saved value holds that id; then it gets a fresh
  identity above the saved counter, assigned in setup's rendering order. A
  setup that replays the saving State's initialization renders each such
  value with the id its counterpart had, so the loaded State continues as the
  saving one would. A setup that differs can print an id that a saved value
  also shows after the load, or that a value collected before the save had
  shown; no two live values share an id.
- Duplicate keys in the saved global table and duplicate pins are rejected,
  not silently replaced, as are pins in an unsealed save.
- Strings above `limits::MAX_STRING_BYTES` are rejected, also in saves written
  by earlier builds with the same format version.
- A table's saved logical array length is accepted up to the array limit
  without storage behind it (see [limits](limits.md)). A later write whose
  array growth cannot be reserved, such as one an edited save's impossible
  gap demands, raises the ordinary `table growth failed` error, which kills
  the callback and leaves the table unchanged. A restore step that cannot
  grow a table rejects the load as `InvalidTable` with
  `TableRestoreFailure::GrowthFailed`.
- An older format version is rejected as unsupported before other checks.
- The cost model version must match `COST_MODEL_VERSION` exactly.
- Loading restores the saved cost counters. Hosts must call `set_cost_budget`
  afterwards.
- Allocation during snapshot materialization never runs GC. Its collection
  happens after all globals, deltas and setup anchors are in place. The setup
  callback runs before materialization and may trigger automatic or explicit GC.
- GC policy and the current threshold are host configuration, not snapshot
  data. A loaded State starts with the default automatic policy unless its
  setup callback changes it. If setup calls `gc_set_policy`, the final load
  collection recomputes the threshold from that policy and the loaded live
  allocations. A threshold set with `gc_set_threshold` in setup remains fixed
  across the final collection. See
  [the GC policy](limits.md#garbage-collection-policy) for the policy and
  threshold controls.

## Byte stability

- The codec is little-endian and length-prefixed, with explicit enum tags.
  It is bounds-checked. Initial vector reservations, measured in bytes, are
  capped at the remaining input; vectors then grow as elements are decoded.
- Two saves of the same logical state are byte-identical with the same crate
  version. The crate version string is part of the blob.
- A table saves its logical array length and the slots from its first live
  array key to its last, never where its stored window happens to sit, so a
  stored array window history does not reach the bytes. Hash slots preserve
  insertion history, storage kind, dead keys and vacant positions, so cleared
  `next` controls and later compaction behave identically after load. Dead keys
  remain GC roots until compaction removes their slots. A loaded State and the
  continuous run it came from keep saving identical bytes.
- A host that content-hashes saves across upgrades must hash the payload after
  the header, or accept that a version bump changes the bytes.
- Saves are user-editable input. Authenticate them if tampering with future
  RNG outcomes matters.
