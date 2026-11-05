# Save state

Details of the `snapshot` feature beyond the README. The format and design are
in the module doc of `src/vm/save_state.rs`.

## What is saved

- User globals, with the reachable tables, closures, upvalues and strings.
- The RNG stream, the State's costing and the cost counters. The costing is
  either metered, with the weight of every charge in `Charge` order, or off.
- State-local object identities that `tostring` and `%p` have rendered,
  including those of the environment objects `open_libs` rebuilds on load.
- The standard-library tables, whole, by the same rule as the global table.

Not saved: call stack, program counter, open upvalues, coroutines, anchors,
callbacks, host user-data. Values reachable only through anchors are omitted
from the saved graph; put game state in globals or a graph reachable from
them if it must survive. `SaveDiagnostics::anchor_count` reports live anchors,
not which values were omitted, and does not make the save fail. Saving is
valid only while the VM is quiescent.
The load setup callback configures the rebuilt environment and function
registrations. Values it leaves on the value stack are discarded before load
returns; the loaded State starts with an empty stack.

## Environment tables

- `math`, `string`, `table`, the metatable of `_G` and the shared string
  metatable are rebuilt by `open_libs` on load and referenced by token. The
  string metatable's token is `string.metatable`.
- The global table, which `_G` names, exists in every State (a raw one
  included). A reference to it is saved as the token `_G` and resolves to the
  loaded State's global table.
- The object-to-token map is refreshed at each successful `open_libs`, not
  read from the live builtin slots. Shadowing a builtin (`math = {}`) cannot
  misclassify the user's table as an environment object. A library table a
  later `open_libs` replaces is an ordinary table from then on.
- The global table and every environment table follow one rule, set out for
  globals below: each is saved whole, the save is authoritative for what it
  holds, and load setup contributes only string keys the save mentions
  nowhere. They share the slot policy that makes this exact (see Sealed
  globals): until sealing they keep every tombstone and a re-added key takes
  its old slot back, and sealing records each table's pins.
- So a load setup value for a library key the save holds is discarded,
  whether or not the script changed that table: a stock function included, a
  host override of `math.floor` made in setup gives way to the saved one.
  Change such a field after `load_state` returns. A setup metatable on a
  library table gives way to the saved one too, as for the global table.
- A later build adding a field to a library table does not corrupt old
  saves: setup contributes the field the save never mentions. A build that
  drops a stock function cannot load a save naming it (`UnknownFunction`), as
  for any registered function id; every save names every stock function.
- A delta against the freshly opened library, the earlier design, could not
  make a load continue the saving State exactly: it cannot tell a key never
  changed from one restored to its stock value after initialization had
  changed it, and whether setup's value won depended on unrelated edits to
  the same table.

## Globals

- The global table is saved whole, like any table: its array part and every
  hash slot in physical order, nil tombstones included, so the loaded table
  iterates and compacts exactly as the saving one would. An unshadowed builtin
  encodes as a token; a shadow is preserved as the user's value. Keys of any
  type are kept, non-UTF-8 strings included.
- The save is authoritative for every key it holds, tombstones included, and
  for the array part and metatable. Load setup contributes only string-keyed
  globals the save mentions nowhere (neither as a slot nor as a pin); they
  follow the saved slots, in setup order. Setup's other keys (numbers,
  booleans, tables, functions) are discarded.
- Each environment table is saved and restored the same way, with its own
  pins.

## Sealed globals

- `State::seal_globals` ends global initialization (see
  [limits](limits.md)). Until then the global table and every environment
  table keep every tombstone. Sealing records each one's pins: every string
  key with a slot at that moment, which is every name initialization wrote,
  including ones it set and then cleared. After sealing, tombstones compact
  as in any table. A library table `open_libs` builds after sealing is pinned
  at once. The rest of this section says "globals", and holds for each
  environment table in the same way.
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

- A Rust function value is an entry of its State's function table, never a
  code address. `State::add_rust_fn` and `State::register_rust_fn` each create
  a new entry and return a `RustFnHandle` that `State::push_rust_fn` pushes;
  every push of one handle is the same value, and two entries are two
  functions even when they wrap the same Rust function. Identity therefore
  does not depend on how the binary was built: the optimizer or linker merging
  identical functions changes no `==` result, table key, rendered identity or
  saved id.
- A reachable Rust function is saved by its entry's registered stable id. One
  without an id (`add_rust_fn`) fails the save.
- An id names one entry, and registering an id the State already has fails
  with `ErrorKind::DuplicateFunctionRegistration`, whatever function it
  names. Registration reports through the crate's `Error`, like every other
  `State` operation, so a `load_state` setup closure propagates it with `?`
  into its own error type. To install
  one registered function under several names, push its handle for each.
- The standard library's functions, including the iterator callbacks library
  functions return, are fixed catalog entries in every State. `open_libs` gives
  them their ids under dotted paths (`math.sin`, `ipairs.iter`, ...); global
  `unpack` and `table.unpack` are separate entries with their own ids. A raw
  State may register those ids for host functions before opening the
  libraries; `open_libs` then refuses with the smallest conflicting id before
  installing anything, leaving the stack, globals, heap and registry
  unchanged. Reopening the libraries binds each id to its own entry again.
- A save records the rendered identity (`tostring`, `%p`) of every registered
  Rust function that has been printed, including one only the host still
  holds. Passing it back in after a load prints the same `function: 0x...`.
- On load, a recorded function that setup no longer registers is skipped.
- A handle is bound to the State that issued it; a loaded State's setup
  creates its own handles, by registering the same ids. A function added
  without an id has no identity across a save: setup can add it again, but
  that is a new function, and its old rendered identity is not restored.

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
- A format version other than `FORMAT_VERSION` is rejected while reading the
  header, before payload decoding.
- The cost model version must match `COST_MODEL_VERSION` exactly.
- The loaded State is built under the saved costing, so setup runs under the
  saved price list. A State's costing never changes, and a load takes it from
  the save, not from the host. Setup starts with fresh counters and no budget;
  when the save is metered it may set a temporary budget of its own (under
  `Costing::Off`, `set_cost_budget` is refused, here as everywhere). After a
  successful load the saved counters replace whatever setup charged or
  budgeted, so a budget that was configured at save time is still enforced.
  For trusted saves taken without a budget, call `set_cost_budget` afterwards.
- Untrusted bytes can select Off mode or arbitrary weights, including all
  zeros. The cost-model version checks the schema, not the host's price list.
  Compare `State::costing()` with the expected `Costing` at the start of setup,
  before executing scripts, and reject a mismatch through `LoadError::Setup`.
  For example, a setup closure can begin with:

  ```rust,ignore
  if state.costing() != &expected_costing {
      return Err("unexpected saved costing");
  }
  ```

  The comparison includes the mode and every weight. After a successful load,
  call `set_cost_budget` and handle its result to replace the saved budget and
  reset use. Setting a budget in setup is insufficient because materialization
  replaces it with the saved counters. A budget alone cannot enforce weights
  edited to zero; under Off mode, setting one is refused.
- The payload stores a costing tag, then for a metered save one u32 weight per
  charge in `Charge` discriminant order. The count follows
  `COST_MODEL_VERSION`, which the header matches exactly. An off save stores
  only the tag. Decoding is strict: an unknown tag, a truncated weight list,
  and an off save with use or a budget are rejected as decode errors.
- Allocation during snapshot materialization never runs GC. Its collection
  happens after all globals, environment tables and setup anchors are in
  place. The setup
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
- Two saves of the same logical state written by the same crate version are
  byte-identical, whichever build profile compiled it: no address or build
  option reaches the bytes. The header carries the crate version string, so
  two crate versions write different headers even for an identical payload.
- A table saves its logical array length and the slots from its first live
  array key to its last, never where its stored window happens to sit, so a
  stored array window history does not reach the bytes. Hash slots preserve
  insertion history, storage kind, dead keys and vacant positions, so cleared
  `next` controls and later compaction behave identically after load. Dead keys
  remain GC roots until compaction removes their slots. A loaded State and the
  continuous run it came from keep saving identical bytes. Rendered pointer
  identity records are written in ascending id order: loading can merge setup
  identities into the runtime map in a different insertion order, but that
  order does not affect the encoded bytes. Resaving a State whose runtime map
  order was not ascending may therefore produce different bytes from older
  builds, while retaining the same format and identities.
- A host that content-hashes saves across upgrades must hash the payload after
  the header, or accept that a version bump changes the bytes.
- Saves are user-editable input. Authenticate them if tampering with future
  RNG outcomes matters.

## Format release policy and golden review

An unreleased format may change before shipping. Once it ships, a change to
encoded fields, tags, persisted opcode encodings or iterator schemas needs a
new format version. Compiler output, engine metadata and cost-model changes
can change save bytes without changing the format.

The released-format list in `tests/integration/snapshot/save_golden.rs` records
every released format, each with the release tree that establishes it (named
in the comment above the list); releases before 0.4.0 predate the snapshot
feature. Their original binary fixtures are archived with fingerprints. They are historical evidence, not supported load formats.
At every release, archive the current golden for each new
(format version, cost-model version) pair and add its file name, both version
words and fingerprint to that list. Use `save_released_v<format>_cost<cost>.bin`
for new archives; the original archive names stay unchanged. A cost-model-only
release needs a new archive even when its format is unchanged. The current
golden's header is not pinned, so regenerate it at the release commit
first: the archive must carry that release's engine label and
cost-model word. Never regenerate a released fixture. While a
listed fixture's format and cost-model versions are the current ones, the
check loads it with the current build, saves it again and requires the same
payload (everything after the engine label). Historical pairs still have
their fingerprints and header words checked; a diagnostic explains when the
load check cannot run. A change in how saves are read or written therefore
fails it; compiler, registration and golden-program
changes do not, because they leave the archived save alone. Deliberate edits
to the list or fingerprints still require compatibility review.

At release, also archive `opcode_encoding.txt` as
`tests/fixtures/opcode_released_v<format>_cost<cost>.txt` and pin its XXH3-64 digest in
`persisted_encodings_match_fixture` in `src/instr.rs`. While that format
and cost model are current, require equality with the current encoding fixture
as well as checking the archive digest. When only the cost model changes,
archive another encoding fixture for that pair and review its diff: cost
classes and the cost-model header may change, but persisted opcode numbers,
layouts and builtin ids still require a new format. The original format's record is its
release-tree instruction source, because it predates the encoding fixture;
the next released format has its original encoding fixture. These records are
historical evidence and must never be regenerated from current declarations.

The current golden pins the save payload after the header, with the format
word checked separately. The payload is the same in every build profile, and
the `release-goldens` check shape in `brokkr.toml` holds an optimized
build to the fixture alongside the dev sweeps. Engine and cost-model header metadata do not
participate in this pin. The load probe uses the current cost-model header on
this payload; released archives always retain strict, original headers.
The generated text companion lists string, bytecode, upvalue and object arenas
in id order, table slots in storage order, globals, environment tables and rendered
identities. Each bytecode record is summarized by encoded length and FNV-1a
hash so compiler changes stay local in the diff. The reader is test tooling,
not the production decoder; complete consumption is checked. Both the
regenerator and the golden test use the same reader. Run
`brokkr run --features snapshot regenerate_save_golden` (any profile) to
update both, and inspect the text diff before accepting the change. Besides
format and traversal changes, compiler output, standard-library function ids,
the golden program and the charges it incurs (a cost-model change) all change
the payload. Deterministic traversal reorders are
replay-visible, but do not by themselves make old saves unreadable. Golden
regeneration is not evidence that a format change is compatible.
