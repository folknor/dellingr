# Save state

Details of the `snapshot` feature beyond the README. The format and design are
in the module doc of `src/vm/save_state.rs`.

## What is saved

- User globals, with the reachable tables, closures, upvalues and strings.
- The RNG stream and the cost counters.
- State-local object identities that `tostring` and `%p` have rendered.
- Standard-library tables as deltas against the pristine environment.

Not saved: call stack, program counter, open upvalues, coroutines, anchors,
callbacks, host user-data. Saving is valid only while the VM is quiescent.

## Environment objects and deltas

- `math`, `string`, `table`, `_G`, the metatable of `_G` and the shared string
  metatable are rebuilt by `open_libs` on load and referenced by token. The
  string metatable's token is `string.metatable`.
- The object-to-token map is refreshed at each successful `open_libs`, not
  read from the live builtin slots. Shadowing a builtin (`math = {}`) cannot
  misclassify the user's table as an environment object.
- Per environment table, a delta records:
  - keys deleted from the baseline;
  - keys added or changed;
  - the key order, only when replaying the first two would not reproduce it;
  - the whole logical array part, nil slots included, when there is one
    (ordinary tables save only the span from their first live array key to
    their last, plus the logical length; see Byte stability);
  - hash-slot history when it differs from the baseline, including dead keys,
    vacant positions and the map storage marker;
  - the metatable: unchanged, cleared, or replaced.
- The array shape is saved because it decides where later integer writes
  land.
- Adding a field to a library table in a later build does not corrupt old
  saves.
- Globals are saved whole, in binding order, nil tombstones included. An
  unshadowed builtin encodes as a token; a shadow is preserved as the user's
  value.

## Sealed globals

- `State::seal_globals` ends global initialization (see
  [limits](limits.md)). Bindings written before it are pinned: their nil
  tombstones are never compacted, so a deleted name that load setup reinstalls
  stays deleted after the load.
- The save records whether the State was sealed. The loaded State takes that
  phase: a call to `seal_globals` inside the load setup is deferred to it.
- Pins are not saved. A sealed load pins exactly the names its setup left
  bound or tombstoned, so the setup must replay the initialization the saving
  State performed before sealing, including names it set and then cleared.
  Under that contract the loaded State compacts exactly when the saving one
  would, and later saves of both stay byte-identical. A setup that differs
  re-derives the pin set from the new setup.
- Loading rebuilds the bindings in saved order. Names the setup installed that
  the save does not mention follow the saved ones, in setup order.

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
  snapshot bytes are only guaranteed identical across the same build, and a
  folded pair loads back as the smaller id's function.

## Load

- Setup runs inside `load_state`; its closure returns `Result<(), E>`, surfaced
  as `LoadError::Setup(E)`. Anchors created there survive.
- Duplicate saved global names are rejected, not silently replaced.
- Strings above `limits::MAX_STRING_BYTES` are rejected, also in saves written
  by earlier builds with the same format version.
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
