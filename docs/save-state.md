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
  - the whole array part, nil slots included, when there is one;
  - the metatable: unchanged, cleared, or replaced.
- The array shape is saved because it decides where later integer writes
  land.
- Adding a field to a library table in a later build does not corrupt old
  saves.
- Globals are saved whole. An unshadowed builtin encodes as a token; a shadow
  is preserved as the user's value.

## Numbers

- Numbers are written by bit pattern. `-0.0` is exact.
- Every NaN is written as one canonical quiet NaN, so host differences in NaN
  sign or payload do not change snapshot bytes. This also applies to NaN
  literals in bytecode. No script can observe the difference.

## Rust functions

- A reachable `RustFunc` is saved by its registered stable id. An unregistered
  one fails the save.
- The standard library registers itself under dotted paths (`math.sin`, ...)
  during `open_libs`. `open_libs` rejects conflicting registrations before
  installing anything, leaving the stack, globals, heap and registry unchanged.
- A save records the rendered identity (`tostring`, `%p`) of every registered
  Rust function that has been printed, including one only the host still
  holds. Passing it back in after a load prints the same `function: 0x...`.
- On load, a recorded function that setup no longer registers is skipped.

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
- Allocation during load never runs GC. The only collection happens after all
  globals, deltas and setup anchors are in place.

## Byte stability

- The codec is little-endian and length-prefixed, with explicit enum tags.
  It is bounds-checked, and reservations are capped at the remaining bytes.
- Two saves of the same logical state are byte-identical with the same crate
  version. The crate version string is part of the blob.
- A host that content-hashes saves across upgrades must hash the payload after
  the header, or accept that a version bump changes the bytes.
- Saves are user-editable input. Authenticate them if tampering with future
  RNG outcomes matters.
