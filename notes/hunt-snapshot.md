# Verification hunt: snapshot

Scope: `tests/save_state.rs`, `save_state_builtin_alias.rs`,
`save_state_error_paths.rs`, `save_state_identity.rs`, `save_golden.rs`,
`ipairs_save_state.rs`, `tests/fixtures/save_golden_current.bin` with
`tests/common/save_golden_program.rs` and `examples/regenerate_save_golden.rs`,
`src/vm/save_state.rs` and `src/vm/save_state/` (`verify.rs`,
`integrity_tests.rs`), `src/snapshot_format.rs`, and how the `snapshot` feature
is exercised by the `brokkr.toml` sweeps.

Nothing was run. Statements about behaviour come from reading the code, the
git history, and the last `brokkr check` log and journal under `.brokkr/`.
Where a claim needs a run to confirm, it says so.

Findings are grouped by the brief's five questions, unranked. Each ends with
whether the fixed version can be held mechanically.

---

## 1. Tests that prove nothing (or less than they say)

### 1.1 The save golden pins a build that production never runs

`tests/save_golden.rs` compares `golden_bytes()` with the fixture byte for
byte. Its own header says the fixture "must come from the profile the tests
run under": a release build can fold `base_unpack_values` and
`table_unpack_values` (two byte-identical wrappers around `unpack_values` in
`src/lua_std/basic.rs` and `src/lua_std/table.rs`), and the save then writes
the lexicographically smallest id (`table.unpack`) for the global `unpack`.
The golden program loads the standard library, so every builtin global is in
the save as an `Fn` token and the folding reaches the bytes.

So the test asserts the byte stream of a debug build. The bytes a shipped
(release, `lto = "fat"`) game writes are not pinned by anything. The test
reads as "save bytes are pinned", and the pinned thing is a dev artifact.

Nothing in the test detects which profile it runs under. Run under release, it
would fail with "the walker reordered the object graph and every existing save
file is now unreadable", which is the wrong diagnosis.

That can happen today without anyone choosing it. `brokkr test` drops each
sweep's `tests = [...]` filter (`decide_sweeps` in brokkr's
`src/test_cmd/plan.rs` resets `s.filters`) and honours a sweep's own
`profile` ahead of `[test] debug`. So `brokkr test save_traversal_order` should
also run in the `release-native-stack-snapshot` sweep, in release, with
`--features snapshot`. Whether LLVM actually merges the two wrappers there
needs one run to confirm. If it does, a focused run of the golden test fails
in one of its two sweeps.

Fix, in order of preference:
- Remove address identity from Rust functions entirely (see 4.1). Then no
  build can fold two ids into one and the golden is profile-independent.
- Failing that, take address-dependent content out of the golden. Build the
  golden State with `State::raw()` plus only the host-registered functions
  the program needs, so no foldable pair is reachable. Or mark the test
  `#[cfg_attr(not(debug_assertions), ignore = "...")]` so a release run says
  why it skipped instead of misdiagnosing.

Enforceable: the first fix makes the hazard unrepresentable. The `cfg_attr`
fallback is a one-off and cannot be held by a rule.

### 1.2 The golden is entangled with everything except what it claims to guard

The header says the fixture "pins the save codec's traversal order against a
byte fixture captured before the iterative-walker rewrite". In practice it
pins the whole byte stream, and that stream contains:

- `CARGO_PKG_VERSION`, written right after the version words
  (`save_state()` writes it as `SnapshotField::EngineVersion`);
- `COST_MODEL_VERSION`;
- the compiler's full output for `GOLDEN_PROGRAM`: instruction words,
  register allocation, `line_info`, `defined_line`, cache-slot counts;
- every stdlib registration id and the order `open_libs` installs builtins;
- the debug-profile function addresses (1.1).

History shows what that costs. Of the 16 commits that touched
`save_golden_current.bin`, these 8 did not change `FORMAT_VERSION`: 7ee46fd,
03b789c, d278462, 8d3151a, 2d01058, 6c795d1, f81e98e and 644b104.
- 6c795d1 ("Release 0.5.0") rewrote it at the same 1793 bytes: the crate
  version string changed and nothing else did.
- f81e98e (per-operator line attribution) rewrote it at the same size:
  `line_info` changed.
- 03b789c and d278462 are cost-model bumps.

So the fixture gets regenerated whenever the compiler, the stdlib, the cost
model or the release number moves. That trains whoever does it to run
`regenerate_save_golden` by reflex. A real traversal reorder made in the same
commit as any of those changes is blessed along with them. The test can only
catch a reorder made in a commit that touches nothing else, which is the case
that least needs catching.

The fixture is also binary, so `git diff` shows `Bin 1876 -> 1884 bytes` and
review cannot see what changed.

Fix:
- Compare the payload after the header, not the whole blob. The module doc
  already tells hosts to do exactly this for content hashing.
- Replace the binary golden with a text dump produced by a test-only decoder,
  e.g. `tests/fixtures/save_golden_current.txt`. List the arena in id order:
  `obj 3 table keys=[str 5 -> obj 4, ...] mt=obj 7`,
  `closure 2 chunk 1 upvalues=[0, 1]`. Print bytecode chunks as a hash plus a
  length, not their words. Then a reorder shows up as a readable diff, and a
  codegen change only touches one hash line, or none if the chunk is
  summarized by its shape rather than its contents.
- Keep a separate, small binary golden built from a raw State with no
  closures. That one pins exact codec bytes without codegen noise.

Enforceable: yes, structurally. With the header excluded and bytecode
summarized, the golden stops depending on things it should not. A
`test_conventions`-style check can require every `tests/fixtures/*.bin` to
have a sibling `.txt` rendering that the same test asserts.

### 1.3 Regenerating the golden does not require a format bump, so it cannot hold the line it was built for

The test's first assertion compares `actual[4..6]` with `expected[4..6]`, the
version words of the current build and of the fixture. Regeneration rewrites
both sides at once, so a change to the encoded layout passes as soon as the
fixture is regenerated, whether or not `FORMAT_VERSION` moved.

644b104 is the worked example. It added a `defined_line: u32` field to every
saved function in `SavedBytecode::encode` and `decode`, and changed the
`OP_CALL` C operand. `FORMAT_VERSION` did not move: the commit message says
the change was "inside the still-unreleased format 10". That policy is
reasonable, but it lives only in commit messages. Nothing in the tree records
which format versions shipped, so nothing can tell "unreleased, may still
change" from "released, frozen".

Fix: freeze one golden per released format, e.g.
`tests/fixtures/save_v13.bin` (header-stripped). Keep a list of released
versions in the test (`RELEASED_FORMATS: &[u16]`). The test then asserts:
- every released version has a frozen fixture;
- a frozen fixture is never rewritten, by pinning each one's xxh128 in the
  test, the same way `brokkr.toml` pins workloads;
- if `FORMAT_VERSION` is in `RELEASED_FORMATS`, the current save of the
  golden program equals the frozen one.

The release process (CHANGELOG plus the version bump) appends to the list.

Enforceable: yes, as a test. Editing a pinned hash stays possible, but it
becomes a deliberate, visible act, which is the same bargain `brokkr.toml`
already makes.

### 1.4 The `snapshot` block of `persisted_encodings_match_fixture` cannot fail on version

In `src/instr.rs`, `persisted_encodings_match_fixture` first asserts that the
fixture's first line equals `format={FORMAT_VERSION}`. Under `snapshot` it
then asserts that `State::new().save_state()` writes the same number at bytes
4..6. The encoder writes `FORMAT_VERSION` there (`write_magic_and_versions`),
so both sides come from one constant. The block can only fail if the header
layout moves, which its message ("review the encoding fixture when changing
the snapshot format") does not describe.

The fixture's useful direction is real: bumping `FORMAT_VERSION` forces an
edit to the fixture's first line. The other direction is not enforced.
Changing an opcode number or a cache encoding forces a fixture edit, but not
a version bump. In history every fixture edit after fcc1f9e did coincide with
a bump (41ad774, bc4f1bf, 307b5eb, 7204148, 2bcc71e, f692896, f8a4704), so
discipline has held so far. Nothing holds it.

Fix: give `opcode_encoding.txt` the same per-released-version freezing as 1.3.
Its pinned digest is keyed by version, so the encoding cannot change under a
released version. Drop the tautological header block, or reword its message
to say what it actually checks: the header layout.

Enforceable: yes, as in 1.3.

### 1.5 `saves_are_byte_stable` checks only that saving is pure

It saves one State twice in a row and compares the two. That fails only if
`save_state` mutates the State, and `save_state` takes `&self`. The test name
reads as "saves are deterministic". The determinism that matters is across
processes, address-space layouts and slotmap histories:
- `object_ids`, `string_ids` and `env_reverse` are `BTreeMap`s keyed by
  `ObjectPtr`/`StringPtr`;
- `rust_fn_ids_by_addr` is keyed by function address.

Only the golden exercises cross-process stability, and only in debug (1.1).

Fix: replace it with a test that builds the same logical state twice by
different routes, so the slotmap keys differ, and compares bytes. Example:
State A allocates and drops 1000 throwaway tables first, then builds the
graph; State B builds it directly. Do the same for strings and closures. That
catches any future map iteration over pointer order.

Enforceable: the test is the enforcement. Separately, a clippy
`disallowed_types`-style ban cannot express "do not iterate a pointer-keyed
map in the save walker". The code comment at `finish` ("Address keys are
lookup-only") is the only guard, and it is unenforced.

### 1.6 `deep_global_chain_saves_and_loads_iteratively` proves iteration only for the ambient stack size

It saves and loads a 50000-deep chain on the libtest worker thread. Whether a
recursive walker would overflow depends on that thread's stack size:
- libtest's default is 2 MiB;
- `RUST_MIN_STACK` in the environment raises it;
- release frames are smaller than dev frames.

A regression to recursion could pass on a host with `RUST_MIN_STACK` set, or
in a profile with small frames. The test also computes `done` and never reads
it.

Fix: run the body on a thread with an explicit small stack, e.g. 256 KiB via
`std::thread::Builder::stack_size`. `tests/common/stack.rs` already owns
thread sizing for `native_stack`. Then assert on `done`.

Enforceable: as a test, once written that way. A convention check could
require that any test named `*iterative*` spawns its own thread, but that is
keyed on a name and would fail open.

### 1.7 `save_walker_object_count_matches_gc_live_minus_env` claims more than it checks

The test comment says "Adding a GC root without teaching the save walker
would break this identity." That holds only if the test's program puts an
object into the new root. Anchors are the counterexample already in the tree:
they are a GC root the walker deliberately does not follow, and the identity
holds only because this program creates no anchor. A future root category
(another transient root, a cache that pins an object) passes this test
unless someone also edits the program.

Fix: make the relation structural. `vm::mark_gc_roots` is already the single
source of truth for roots. Give it a root-category enum that the save walker
must `match` exhaustively, with each arm either "walked", "saved by token" or
"deliberately not saved: reason". A new root then fails to compile until the
walker classifies it.

Enforceable: yes, by the type system (exhaustive match), which is the
strongest option available.

### 1.8 `reset_and_quiescence_share_the_snapshot_inventory` tests a subset of a hand list

The `in_flight!` macro in `src/vm.rs` builds `validate_quiescent` and
`reset_in_flight` from one list, which is good. The test dirties only some of
the listed fields:
- dirtied: `stack_bottom`, `pending_bytecode_caches`, the transient roots,
  the three depths;
- not dirtied: `open_upvalues`, `call_stack`, `vararg_stack`,
  `active_rust_fn`.

More importantly, nothing forces a new in-flight field of `State` into the
macro list at all. Pooled `frame_stacks` is a candidate worth a look. The
test name says "share the inventory", and the inventory is what goes
unchecked.

Fix: move the in-flight fields into one `InFlight` sub-struct of `State` with
`#[derive(Default, PartialEq)]`. Quiescence is then `self.in_flight ==
InFlight::default()`, and reset is `= InFlight::default()`. A new field is
covered by construction, and the test reduces to "default is quiescent".

Enforceable: yes, structurally (a type).

### 1.9 `literal_identity_strings_follow_chunk_ids_instead_of_addresses` cannot tell its claim apart from a reverse-address sort

The test orders chunks by descending `Arc` address, encodes them in that
order and expects the literals in the same order. It catches an
implementation that sorts by ascending address. An implementation that sorts
by descending address also passes.

Fix: run the scenario twice with the encode order flipped, and assert the
output follows the encode order both times.

Enforceable: as a test only.

### 1.10 `tombstones_are_preserved_in_snapshots` asserts "longer", not "contains the history"

`tombstoned_bytes.len() > clean_bytes.len()` passes whenever the tombstoned
save has any extra byte. In case 1 that extra byte is already supplied by the
dead key's string `"b"`, which enters the string pool whether or not slot
history is encoded. The pairs-order half of the test is what carries the
weight.

Fix: decode both saves (the test-side decoder from 1.2 would serve) and
assert the dead slot itself is present.

Enforceable: as a test only.

### 1.11 `corruption_sweep_never_panics` covers one mutation of one small save

It truncates at every prefix and XORs `0xff` at every offset of one save of a
four-global program. Limits:
- One mutation kind. Single-bit flips, length fields set to `u32::MAX`, and
  swapped ids are not tried.
- Many flips land in the `EngineVersion` string, which load reads and
  discards, so they test nothing.
- A flip that loads successfully is never exercised afterwards. A forged save
  that loads and then breaks an interpreter invariant on first use (the
  verifier's whole purpose) is invisible to the sweep.
- The comment says "never panics or OOMs". An OOM aborts the process, and
  nothing bounds or measures allocation.

Cost per run: about 2x the save length in `load_state` calls (the golden
program, which is similar, saves 2467 bytes, so roughly 5000 loads). Each
successful flip builds a full standard State in debug. This is unmeasured;
`brokkr test corruption_sweep --timings` would give the number.

Fix:
- After each successful mutated load, call every global function, iterate
  every table with `pairs`, then `save_state`.
- Run the sweep over a corpus of saves (see 3.3) rather than one.
- Move open-ended mutation to a `cargo fuzz` target fed by a grammar-aware
  mutator over `SavePayload`, which can mutate decoded fields rather than
  raw bytes.

Enforceable: the in-check sweep is a test. A fuzz target cannot be a
`brokkr check` gate, but a fixed-seed, bounded mutator can.

### 1.12 `examples/save_roundtrip.lua` never saves

It is the Lua twin of `round_trip_preserves_data_graph_and_env_references`,
with the save removed. It prints `save roundtrip shape: true`. The CLI has no
snapshot path (`src/main.rs` has no `cfg(feature = "snapshot")`), and nothing
references the file (`git grep save_roundtrip` is empty). It runs in
`run_examples` and in the diff gate in both sweeps, and proves nothing about
saving. Its name says otherwise.

Fix: delete it, or make it real by giving the corpus a save round trip
(3.3), at which point every example covers this.

Enforceable: once 3.3 exists, a file named for a save scenario is redundant
by construction. No rule would catch the name on its own.

### 1.13 Smaller overclaims in test names and messages

- `every_save_fixture_is_declared_and_referenced` checks "declared" only.
  `FIXTURE_NAMES` is a second spelling of the file named by
  `save_golden_program::GOLDEN_FIXTURE`. Rename the fixture constant and
  `FIXTURE_NAMES` still names the old file. Derive one from the other.
- `save_traversal_order_matches_golden_fixture`'s failure message says "every
  existing save file is now unreadable". A pure traversal reorder does not
  make old saves unreadable: ids are dense indices, and the loader
  materializes whatever order it is given. The real consequences are changed
  bytes (content-hashing hosts, replay diffs) and a format change that
  skipped its version bump. The message points the reader at the wrong
  failure.
- `saved_main_chunk_keeps_zero_defined_line` round-trips
  `valid_saved_bytecode()` through the codec. It asserts nothing about main
  chunks or line 0 beyond what `saved_bytecode_round_trips` already covers.
- The `GOLDEN_PROGRAM` doc says it "exercises every edge kind the walker can
  take". It has:
  - no sealed globals, so no pins;
  - no tombstones;
  - no non-string global keys;
  - no two closures sharing one upvalue cell;
  - no gmatch iterator;
  - no environment-delta array span or hash slots;
  - no metatable delta;
  - no rendered `Fn` identity.

  That claim is false today. It can be made checkable: decode the golden and
  assert each `SavedVal`, `SavedObject` and payload-section variant appears at
  least once, using an exhaustive match so a new variant fails to compile
  until the golden covers it.

Enforceable: the last one, yes (exhaustive match in a test). The others are
one-off tidy-ups.

---

## 2. Guards and claims that have stopped holding

### 2.1 Stale and contradictory durable claims

- `tests/save_golden.rs` header: "captured before the iterative-walker
  rewrite". The fixture has been regenerated 15 times since e4bb8de. **False
  today.** Reword to say what it pins now; see 1.2 for what it should pin.
- `docs/save-state.md` "Byte stability" says "Two saves of the same logical
  state are byte-identical with the same crate version." The same file's
  "Rust functions" section says "snapshot bytes are only guaranteed identical
  across the same build", and the module doc of `src/vm/save_state.rs`
  repeats the crate-version wording. They contradict each other, and **the
  crate-version wording is false**: debug and release builds of one version
  differ (1.1). The module doc's "because the diagnostic crate-version string
  is part of the blob" also inverts the logic: the string makes saves differ
  across versions, it does not make them identical within one.
- `docs/save-state.md` "Load": "An older format version is rejected as
  unsupported." Any mismatch is rejected, newer included
  (`read_magic_and_versions`, and `unsupported_format_versions_fail_before_setup`
  tests both directions). The README says the same. Minor, but it suggests a
  forward-compatibility rule that does not exist.
- `docs/determinism.md` "Inline caches": a State resumed from a snapshot
  "continues exactly as the State that saved it would have". Checkable, and
  checked only for gmatch charges (`tests/cost_determinism.rs`) and for output
  and bytes in a handful of save tests. No test compares `cost_used()` after a
  continuation that passes through cold caches, against a continuous run.
  3.3's corpus round trip would hold it for the whole corpus.
- `src/snapshot_format.rs`: "Change this only when encoded fields change".
  644b104 changed encoded fields without changing it (1.3), under an
  "unreleased" policy the file does not mention. Either the comment states the
  policy or 1.3's released-version list enforces it.

Enforceable: the contradictions are a one-off rewrite. The underlying facts
become enforceable through 1.3 and 3.3.

### 2.2 Name-keyed recognition of gmatch iterators

The verifier recognises a gmatch iterator closure by its upvalue holding
`SavedVal::Fn(gmatch_schema::ITERATOR_ID or EMPTY_ITERATOR_ID)`
(`VerifiedSavePayload::gmatch_iterator_chunks`, `validate_gmatch_iterators`).
`gmatch_schema` is shared by the stdlib registration and the verifier, so the
two cannot drift by spelling.

The guard still fails open in one direction. If the stdlib ever stops
registering the iterator under that constant, for example because the
iterator becomes a different Rust function or the wrapper stops capturing it
as an upvalue, then no saved closure matches. `validate_gmatch_iterators`
silently validates nothing, and the forged-state checks it exists for go dark
with no error.

Fix: add a test that saves a live gmatch iterator and asserts
`gmatch_iterator_chunks()` is non-empty for that payload. Better, structural:
make the gmatch iterator's saved form its own `SavedObject` variant instead
of a closure plus a recognisable upvalue. Then the verifier reaches it by
type, not by a name.

Enforceable: the test is a cheap guard. The dedicated variant makes the
recognition structural.

### 2.3 Four hand-written copies of the save header

The header (`write_magic_and_versions` plus the `CARGO_PKG_VERSION` field) is
written in four places:
- production `save_state()`;
- `integrity_tests::encode_fixture`;
- `tests::load_bytecode_with_globals`;
- `tests::forged_literal_save`.

Each test copy restates the layout. If the header changes, say by dropping
the engine version (which 1.2 recommends excluding from comparisons) or
adding a field, the test copies keep producing the old shape. Their failures
would then read as decoder rejections of the fixture, not as stale fixtures.

`SavePayload { .. }` literals are similarly hand-built in five places (those
three, `integrity_tests::empty_payload` and `verify.rs`'s `payload` and
`gmatch_payload`). Every new payload field touches all five.

Fix: add one `fn encode_save(payload: &SavePayload) -> Result<Vec<u8>,
SaveError>` that `save_state` and every test call, and one
`SavePayload::empty(standard_env: bool)` test constructor.

Enforceable: a textlint rule allowing `write_magic_and_versions(` only inside
`encode_save` would hold it mechanically.

### 2.4 `required-features` versus `#![cfg(feature = "snapshot")]`

Six integration files in scope are gated by an inner
`#![cfg(feature = "snapshot")]` instead of a `[[test]] required-features`
entry. In the default sweep, cargo still compiles, links and spawns each one
as an empty harness. The last check journal shows `save_golden`,
`save_state` and the others in lane 0 with `"suite_started","test_count":0`.
That is a silent zero-test pass for a target whose whole name promises
coverage.

The example `regenerate_save_golden` already uses `required-features`, so the
repo has both conventions.

See 3.1 for the cost and the fix. Enforceable: yes. A `tests/test_conventions.rs`
rule can reject a test file whose first item is `#![cfg(feature = ...)]` and
require the `[[test]]` entry instead (one string check, the same shape as
the existing marker checks).

---

## 3. The suite as a system

### 3.1 Empty snapshot binaries in the default sweep, and six binaries where one would do

In the default sweep, `save_golden`, `save_state`, `save_state_identity`,
`save_state_error_paths`, `save_state_builtin_alias` and `ipairs_save_state`
each compile to an empty test binary. Clippy `--all-targets` compiles them
too. Each is linked against the debug `dellingr` rlib and spawned once. In
the snapshot sweep they are six separate link steps with six duplicated
helper sets (`run`, `global_str`, `no_setup`, `reload`, `fresh`).

Cost per run: unmeasured. Six debug test-binary links per sweep is typically
in the seconds once the rlib changes, and it recurs on every check after a
library edit.

Fix: merge them into one `tests/snapshot/main.rs` with modules (`golden`,
`round_trip`, `identity`, `errors`, `iterators`), declared once as

```toml
[[test]]
name = "snapshot"
path = "tests/snapshot/main.rs"
required-features = ["snapshot"]
```

Cargo then skips the target in the default sweep instead of building an
empty one. The snapshot sweep links one binary instead of six, and the
helpers exist once. `test_conventions.rs` would need to scan subdirectories,
which `collect_rust_files` already does.

Enforceable: yes. 2.4's convention rule, plus a rule that a test file using
`save_state(`/`load_state(` lives under `tests/snapshot/` or carries a
per-item `cfg`, keeps snapshot tests from re-scattering.

### 3.2 The snapshot sweep repeats the default sweep and adds little save coverage

The last check log shows 968 tests in `default` and 1145 in `snapshot`. So
the snapshot sweep re-runs the whole default suite, including `run_examples`
and the diff gate (with the reference Luas re-run, whose output cannot depend
on dellingr's features), and adds about 177 snapshot tests. The
`brokkr.toml` comment justifies the sweep by the `cfg(not(feature =
"snapshot"))` sides (stdlib registration, `_G` metatable, RNG naming). That
is true for clippy and compile. For the test run it means 968 tests whose
behaviour mostly cannot differ between the shapes.

The test phase was 55.2 s across four sweeps. The split per sweep is not in
the log, but the snapshot sweep is plausibly close to half.

Fix: make the second run of the corpus earn its cost (3.3). The repeat then
checks something the default sweep cannot. The reference-Lua half of the diff
gate should run once per check, not once per feature shape. That belongs to
the corpus-harness hunt, but the snapshot sweep is where the duplication
lands.

Enforceable: `brokkr.toml` can hold it. A sweep can name the tests it runs,
as the release sweeps already do with `tests = [...]`.

### 3.3 No corpus-wide save round trip

All save coverage comes from hand-built graphs in the scoped files plus about
15 scattered `cfg(snapshot)` tests elsewhere. The 150-odd Lua examples, which
exercise most of the VM's object shapes, never pass through `save_state`.

A save bug in a shape nobody hand-wrote is caught only if a game hits it. The
examples include stress tables, closures, upvalue scopes, metatable edge
cases, gmatch, sort and queues.

Fix: under `snapshot`, add a corpus test, or a `run_examples` mode through a
CLI `--save-roundtrip` flag that only exists with the feature. For each
example:
- run the top level;
- `save_state`, `load_state`, re-save, and assert identical bytes;
- if the example defines `_bench`, call it on both the original and the
  loaded State;
- compare outputs, `cost_used()` and a final save.

That one test holds the "continues exactly" claim (2.1), the byte-stability
claim and the verifier's acceptance of everything the compiler emits
(`compiler_saves_contain_no_closure_free_closes` is the one-chunk version of
that today). It also gives 1.11's sweep a real corpus to mutate.

Enforceable: yes. The corpus walker is shared (`tests/common/examples.rs`),
so a new example is covered automatically.

### 3.4 Feature sweeps share one uplifted CLI binary

The check log shows "prepare: sweep 'default': rebuilt before listing -
/media/folk/Banan/cargo/debug/dellingr has other content", and the same for
every sweep. The default and snapshot sweeps share one profile directory, so
each overwrites `target/debug/dellingr`, the path `CARGO_BIN_EXE_dellingr`
names, and the other sweep has to relink before listing.

Cost: a relink per sweep on every check. It is also correct only because
brokkr runs the sweeps one after another. A `brokkr test` in one feature
shape running while a check runs in the other would spawn whichever binary
was linked last. Out of my scope in detail (check machinery), but the
snapshot sweep is what causes it here.

Fix: give each feature shape its own `CARGO_TARGET_DIR` subdirectory (a
brokkr per-sweep setting), or have tests find the binary through the
deps-hashed artifact that cargo reports rather than the uplifted path.

Enforceable: as a `brokkr.toml` convention, if brokkr exposes a per-sweep
target dir.

### 3.5 Process-wide state and clocks

Nothing in scope writes environment variables, installs a global, sleeps or
reads a clock. `Capture` shares its lines through an `Arc<Mutex<_>>` owned
per test. The only ambient dependency is the worker-thread stack size (1.6).
The six binaries can be merged (3.1) without introducing interference.

---

## 4. What the tests reach that they should have been handed

### 4.1 Rust function identity is a code address (production)

`State.rust_fn_ids_by_addr: BTreeMap<usize, String>` keys identity on
`func as usize`. Every save of a `Val::RustFn` goes through it
(`saved_reachable_pointer_value`). The consequences are spread across the
code and the docs:
- debug and release write different bytes (1.1);
- load has a dedicated "folded functions" path (`materialize_payload`) and an
  error for folded keys colliding in one table, with two integrity tests
  (`folded_functions_*`) that simulate a build that cannot be produced on
  demand;
- `docs/save-state.md` needs two paragraphs of caveats.

Rust gives no guarantee that one function has one address. Inlined or
generic functions can be instantiated in several codegen units, and identical
functions can be merged. A host that registers an `#[inline] fn` from one
crate and pushes it from another can hit `UnregisteredFunction` on a function
it did register. No test can catch that, because every test registers and
pushes from the same crate.

Fix (big, and recommended): make `Val::RustFn` carry a registry handle, e.g.
`RustFnId(u32)` into a per-State table of `(stable id, fn pointer)`, assigned
at registration. An unregistered function gets a handle with no stable id,
and saving it still fails fast.
- Identity becomes registration, not address.
- Folding is irrelevant.
- The golden is profile-independent.
- The fold-handling load path, its error variant and its two integrity tests
  can go.
- `rawequal` between two registered ids of one function becomes a design
  decision instead of a linker accident.

It also gives tests the injection point they lack today: a test can register
two distinct ids for one function without simulating a folding build.

Enforceable: by the type system. The bad spelling (`func as usize` as
identity) stops existing.

### 4.2 `CARGO_PKG_VERSION` baked into every save

The engine version string is diagnostic metadata that load discards
(`let _engine_version = ...`). Writing it from `env!("CARGO_PKG_VERSION")`
means every byte-comparing test depends on `Cargo.toml`'s version, and the
release commit has to regenerate the golden (6c795d1).

Fix: exclude the header from golden comparison (1.2). Or let `save_state`
take the engine label from a `State`-level setting (defaulting to the crate
version) that the golden sets to a fixed string. That gives tests an
injection point and leaves production unchanged.

Enforceable: with the header excluded from comparison, the dependency is
gone structurally.

### 4.3 The regeneration example writes through a compile-time path

`examples/regenerate_save_golden.rs` writes to
`env!("CARGO_MANIFEST_DIR")/tests/fixtures/...`, an absolute path fixed at
compile time. Under `brokkr run --commit REF regenerate_save_golden`, brokkr
builds in its own persistent worktree, so the binary would write the fixture
into brokkr's worktree, not the live tree, and report success with
`wrote <path>`.

It also writes unconditionally, so a regeneration with identical bytes still
bumps the mtime. That matters little for a manual tool.

Fix: write relative to the current working directory's repository root, or
take the destination as an argument. Skip the write when the bytes are
unchanged and say so.

Enforceable: no rule needed beyond the fix.

---

## 5. Where the tests assume they run, and what they touch

### 5.1 Profile assumptions

`save_golden.rs` assumes the dev profile without checking (1.1).
`deep_global_chain_saves_and_loads_iteratively` assumes the libtest default
stack and an unset `RUST_MIN_STACK` (1.6). On a differing host or profile,
the first fails with a misleading message and the second silently degrades:
it passes without proving iteration.

### 5.2 Writes

Within scope, the only write is the regeneration example's, into
`tests/fixtures/`, which it owns. See 4.3 for the `--commit` worktree case.
No test writes. `test_conventions.rs` scans only `tests/`, so it does not see
the example's `fs::write`. That is deliberate and fine, but it means the
"file mutations go through scratch" rule says nothing about `examples/`.

### 5.3 Host independence of the codec

The codec is explicitly little-endian, and lengths are `u32`. NaNs are
canonicalised, tested by `nan_sign_does_not_reach_snapshot_bytes` with
hand-built bit patterns, so it does not depend on the host FPU. No locale,
timezone or filesystem behaviour is involved. Nothing here assumes the host
beyond 1.6 and the address identity of 4.1.

---

## Lateral findings

- `load_state` runs `verify::verify_payload` before `decoder.finish()`. A
  forged save with trailing garbage pays for full verification before the
  cheap trailing-bytes rejection. Swap the two calls.
- `src/snapshot_format.rs` sits at the crate root but is mounted as
  `instr::snapshot_format` through `#[path]`, so its location does not match
  its module path. Mounting it as a crate-root `mod snapshot_format;` is
  simpler. The reason for the `instr` mount (the opcode fixture test needs it
  in non-snapshot builds) works just as well from the crate root.
- `open_libs` under `snapshot` builds a whole scratch `State::raw()` and
  re-runs `open_libs_unchecked` whenever the registry is non-empty
  (`check_registry_compatible`), so `with_restricted_env` and any second
  `open_libs` pay a full stdlib install. With 4.1's registry handles, a
  compatibility check could compare id lists without building a State.
- `Cargo.toml` `exclude` keeps `tests/`, so the published crate ships
  `tests/fixtures/crmath_golden.txt` (557 KB) and the save golden. The
  manifest comment justifies `tests/` by the `.lua` corpus, which lives in
  `examples/`.
