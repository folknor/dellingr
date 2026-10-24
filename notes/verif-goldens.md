# Verification: goldens and replay-visible pins

Findings from the verification hunt about the pins on replay-visible output:
the save golden and snapshot format, the opcode-encoding fixture, the cost
model and its version, the RNG draw sequence, library `pairs` order, the crmath
golden, and whether any of them is checked beyond one build on one host. Each
entry names the scopes whose hunters reported it.

```
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
```

---

## VD-001 - The save golden pins everything except what it claims to guard

Reported by: snapshot.

`tests/integration/snapshot/save_golden.rs`'s header says the fixture "pins the save codec's
traversal order against a byte fixture captured before the iterative-walker
rewrite". The fixture has been regenerated 15 times since e4bb8de, so the
"captured before" wording is false today. In practice it pins the whole byte
stream, which contains `CARGO_PKG_VERSION` (written after the version words as
`SnapshotField::EngineVersion`, read and discarded on load), `COST_MODEL_VERSION`,
the compiler's full output for `GOLDEN_PROGRAM` (instruction words, register
allocation, `line_info`, `defined_line`, cache-slot counts), every stdlib
registration id and the order `open_libs` installs builtins, and the
debug-profile function addresses (bugs.md "Save bytes differ between dev and
release builds").

History shows the cost. Of the 16 commits that touched
`save_golden_current.bin`, 8 did not change `FORMAT_VERSION`: 7ee46fd, 03b789c,
d278462, 8d3151a, 2d01058, 6c795d1, f81e98e, 644b104. 6c795d1 ("Release 0.5.0")
rewrote it at the same 1793 bytes because the crate version string changed;
f81e98e (per-operator line attribution) rewrote it because `line_info` changed;
03b789c and d278462 are cost-model bumps. The fixture is regenerated whenever
the compiler, the stdlib, the cost model or the release number moves, which
trains whoever does it to run `regenerate_save_golden` by reflex; a real
traversal reorder made in the same commit as any of those is blessed along with
them. The test can only catch a reorder made in a commit that touches nothing
else. The fixture is binary, so `git diff` shows `Bin 1876 -> 1884 bytes` and
review cannot see what changed. The compiler-cost hunter adds that the failure
message offers only "format or traversal change" while a `COST_MODEL_VERSION`
bump also fails it.

Fix: compare the payload after the header, not the whole blob (the module doc
already tells hosts to do this for content hashing), or let `save_state` take
its engine label from a State-level setting the golden fixes; replace the
binary golden with a text dump from a test-only decoder (e.g.
`tests/fixtures/save_golden_current.txt`, the arena in id order, bytecode chunks
as a hash plus length or a shape summary), so a reorder is a readable diff and a
codegen change touches one line or none; keep a small separate binary golden
from a raw State with no closures for exact codec bytes.

Enforceable: yes, structurally. A `test_conventions`-style check can require
every `tests/fixtures/*.bin` to have a sibling `.txt` rendering the same test
asserts.

## VD-002 - Nothing forces a `FORMAT_VERSION` bump when the encoding changes

Reported by: snapshot.

The save golden's first assertion compares `actual[4..6]` with
`expected[4..6]`, the version words of the current build and of the fixture.
Regeneration rewrites both sides, so a change to the encoded layout passes as
soon as the fixture is regenerated, whether or not `FORMAT_VERSION` moved.
644b104 is the worked example: it added a `defined_line: u32` to every saved
function in `SavedBytecode::encode`/`decode` and changed the `OP_CALL` C
operand without moving `FORMAT_VERSION`, "inside the still-unreleased format
10". That policy lives only in commit messages; nothing in the tree records
which format versions shipped. `src/snapshot_format.rs` says "Change this only
when encoded fields change", which 644b104 did without changing it, under a
policy the file does not mention.

In `src/instr.rs`, `persisted_encodings_match_fixture` first asserts the
fixture's first line equals `format={FORMAT_VERSION}`; under `snapshot` it then
asserts `State::new().save_state()` writes the same number at bytes 4..6. The
encoder writes `FORMAT_VERSION` there (`write_magic_and_versions`), so both
sides come from one constant: the block fails only if the header layout moves,
which its message ("review the encoding fixture when changing the snapshot
format") does not describe. The fixture's useful direction is real (bumping the
version forces an edit to its first line); the other direction is not: changing
an opcode number or cache encoding forces a fixture edit but not a version bump.
Every fixture edit after fcc1f9e did coincide with a bump (41ad774, bc4f1bf,
307b5eb, 7204148, 2bcc71e, f692896, f8a4704), so discipline has held so far;
nothing holds it.

Fix: freeze one header-stripped golden per released format
(`tests/fixtures/save_v13.bin`) and keep `RELEASED_FORMATS: &[u16]` in the
test, asserting that every released version has a frozen fixture, that each
frozen fixture's xxh128 is pinned in the test (as `brokkr.toml` pins workloads),
and that if `FORMAT_VERSION` is released the current save equals the frozen one.
Give `opcode_encoding.txt` the same per-released-version digest. Drop the
tautological header block or reword its message to say it checks the header
layout. Have `snapshot_format.rs` state the unreleased-format policy, or let the
released list enforce it. The release process appends to the list.

Enforceable: yes, as a test; editing a pinned hash stays possible but becomes a
deliberate, visible act.

## VD-003 - The opcode-encoding fixture omits the replay-visible columns

Reported by: compiler-cost.

`tests/fixtures/opcode_encoding.txt` pins opcode numbers, builtin slots, cache
operand encodings and jump encodings against the snapshot `format=`. It does not
pin each opcode's `Layout` (which fixes the reserved bits the verifier rejects,
so it is snapshot-visible) or its `OpcodeCost` class (cost-model-visible).
Reordering layouts or reclassifying costs does not touch the fixture.

Fix: write `OP_ADD_RR=24 Abc Arithmetic` per line and add a `cost_model=<N>`
line, so a cost-class change fails this fixture and names the version that has
to move.

Enforceable: the existing test holds it.

## VD-004 - `COST_MODEL_VERSION` discipline covers 18 sources, and does not force a bump

Reported by: compiler-cost.

`docs/budget.md`: "It moves whenever `cost_used` for identical source changes."
`src/cost_meter.rs` points at `tests/integration/cost/cost_golden.rs` as the check. What it
enforces: `cost_used_matches_golden_corpus` and the error corpus pin 18 sources;
`golden_corpus_fingerprint_matches_cost_model_version` hashes those sources,
their expected costs and the version. Editing an expected cost forces a
fingerprint edit in the same file, and the failure message asks for a version
bump, but nothing requires it: update cost and fingerprint, leave the version,
and every test passes.

Every other cost pin is outside the fingerprint: `stdlib_cost_golden.rs` (every
math/string/table flat charge), `string_charges.rs`
(`basic_native_charges_are_exact`, gmatch caret 26, capture delta 600), the other
tests in `cost_golden.rs` (`table_library_native_charges_are_one_or_zero`,
`unpack_charges_nothing_...`, `mid_run_budget_reset_...`), `bytecode_caches.rs`
(12), `reentrant_opcode_accounting.rs`, the `lib.rs` analysis tests, and
`cost_used` assertions in about a dozen more files (`plain_field_miss`,
`table_iteration`, `table_metamethods`, `global_write_cache`, `error_boundary`,
`panic_poison`, `table_sort_metamethods`, `table_resolution`, `save_state`,
`error_handling`, ...). Changing `math.floor`'s charge means editing
`stdlib_cost_golden.rs`; no test asks for a bump.

Stale facts in the same area: `tests/integration/cost/cost_golden.rs` says "Fingerprint of
`CORPUS` together with `COST_MODEL_VERSION`", but the fingerprint also covers
`ERROR_CORPUS`. `CHANGELOG.md` Unreleased says "`COST_MODEL_VERSION` is 7." in
the register-VM entry and "`COST_MODEL_VERSION` is 8." in a later entry; the
release will ship 8, so the first is false today. Three bumps (6, 7, 8) landed in
one unreleased cycle; a host sees 5 then 8. The entries should say "a
`COST_MODEL_VERSION` bump" and state the final number once.

Fix: one cost golden as data, e.g. `tests/fixtures/cost_golden.txt` with
`cost_model=<N>` on its first line, one row per pinned source, every cost pin
above folded into it. The test fails unless `cost_model=` equals the constant;
the fixture carries `fingerprint=<hash of rows>`, and the test keeps
`COST_MODEL_HISTORY: &[(u16, u64)]`, failing if the current fingerprint appears
under any other version or the current version's recorded fingerprint differs,
so a changed table passes only by appending a new pair with a new number. (An
in-place edit of the last pair stays possible; a script check against `git show
HEAD:` would close it at the cost of a git dependency in the gate.) For the
CHANGELOG, a test that greps the Unreleased section for "`COST_MODEL_VERSION` is
N" and requires N to equal the constant, in the style of
`documented_opcode_charges_match_static_units`.

Enforceable: yes.

## VD-005 - Native-charge lists are hand-mirrored with no completeness check

Reported by: compiler-cost, library.

`docs/budget.md` lists every math function's flat 1, every string and table
function's rule, and the "Free by design" base functions.
`stdlib_cost_golden.rs::every_math_function_has_its_flat_charge` mirrors the
math list by hand; it matches `MATH_FUNCTIONS` today, but a new math function
would go uncovered without any failure. The math `consume_cost(1)` comment says
"every math function charges a flat 1"; nothing enforces it. Nothing iterates
the library tables, though the registrations are declarative arrays
(`lua_std/math.rs`, `string.rs`, `table.rs`; `basic.rs` via `add!`), so adding
`math.tanh` or `table.find` with any charge passes every test. The base-library
free list (`type`, `tonumber`, `tostring`, `print`, `select`, `next`, `pairs`,
`ipairs`, `getmetatable`, `setmetatable`, `rawget`, `rawequal`, `rawlen`,
`error`) has no cost pin at all; only `rawset`, `unpack` and `string.len` are
pinned. A free function that starts charging changes replays and is caught only
by incidental goldens.

Fix: a completeness test that walks `_G`, `math`, `string` and `table` with
`pairs` from a fresh State and requires every function value to have a row in
the cost fixture (VD-004; a free function's row is 0), failing on any function
it has no arity for; optionally check `docs/budget.md`'s lists against the same
fixture.

Enforceable: yes, by the test.

## VD-006 - The dispatch loop's charges are not tied to the opcode cost table

Reported by: compiler-cost.

`src/instr.rs` declares each opcode's cost class once (`opcode_rows!`), and
`static_units()`/`fixed_units()` derive from it; `analyze_cost` uses
`static_units()`; `docs/budget.md`'s table is checked against `fixed_units` by
`documented_opcode_charges_match_static_units`. But the runtime charge in
`Frame::run_frame` is written arm by arm: the six costed table/negation arms use
`add_cost!(const { Instr::fixed_units(OP) })`, the eighteen arithmetic arms use
a literal `add_cost!(1)` inside `arith!`, and the `Free`/`Call` arms charge by
omission. Reclassify `OP_CONCAT` or `OP_LEN`, or change `Arithmetic`'s units, and
`analyze_cost`, the docs test and the opcode fixture follow while the runtime
does not. Nothing checks "runtime charge per executed instruction ==
`static_units()`"; the cost goldens catch only opcodes their sources execute.
The docs claim "These rows describe the units returned by
`Instr::static_units()`" is enforced; the stronger claim implied by
`analyze_cost`'s documentation (that the static walk counts what execution
charges) is not.

Fix, in order of strength: charge `const { Instr::fixed_units(OP) }` in every
arm (folding to zero in the free arms), or generate the dispatch arm skeleton
from `opcode_rows!` so an arm cannot exist without its charge; an
execution-count oracle (a test-infrastructure feature counting executed
instructions per opcode, and a corpus test asserting
`sum(static_units * executed) + native_charges == cost_used`); at minimum a
`frame.rs` unit test that assembles a minimal verified chunk per fixed-class
opcode and asserts the `cost_used` delta equals `fixed_units`.

Enforceable: the first is structural; the others are tests.

## VD-007 - Determinism is pinned on one host, one target and one profile

Reported by: check-machinery, compiler-cost, library.

Replays must be byte-for-byte reproducible "on every host", and goldens exist
for it (`tests/integration/cost/cost_golden.rs`, `tests/fixtures/save_golden_current.bin`,
`tests/fixtures/crmath_golden.txt`, `tests/fixtures/opcode_encoding.txt`). Every
sweep runs them on one host and one target. There is no CI directory.
`examples/hotpath.rs` uses `std::os::unix` and `libc::O_NONBLOCK`, and `cargo
test` builds examples, so the suite cannot build on Windows.

- Profile: every cost test runs only in the dev profile; the release
  `[[check]]` shapes run `tests = ["native_stack"]` only. Release is what ships
  and records replays. `docs/determinism.md` promises the same cost "on a given
  build"; `docs/budget.md` and the `COST_MODEL_VERSION` doc imply
  build-independence and tell hosts they may key stored fingerprints on it.
  Overflow differs between profiles (debug panics, release wraps), and
  `analyze_cost` uses plain `+=` on `u64`. The crmath golden ("the same file must
  pass on every target") also runs only in dev; release (fat LTO,
  `codegen-units = 1`, `#[target_feature]` inlining) never runs it. Rust does not
  contract or reassociate, so the risk is a codegen bug, not semantics. The test
  build's kernel entry also runs an extra thread-local `forced_fma()` read that
  production does not, so the golden exercises a slightly different dispatch
  prologue from the shipped one.
- Target: the aarch64 hardware instance (`fma_dispatch` on `mul_add`, under
  `#[allow(clippy::disallowed_methods)]`) and each kernel's aarch64 `cfg` arm
  (11 copies of about 20 lines of dispatch plumbing, which one `dispatch!`
  macro could replace) are not even compiled or linted on an x86 host. "x87-only
  targets are not supported" is documented, but the code does not refuse to
  build for them.

Fix: a corpus-wide runtime cost golden (read `Cost used: N` from
`run_examples` without `--quiet`, or a library runner), stored in the cost
fixture and run in every `[[check]]` shape including a release one; run the
crmath golden in a release shape (brokkr's `lib_tests = true` selects the lib
unit-test harness with an `only` filter; or the crmath crate split in
rewrites.md); a `[[check]] target =
"x86_64-unknown-linux-musl"` (runs natively; different libc and allocator) and,
with multilib, `i686-unknown-linux-gnu` (32-bit `usize`) entry with a narrow
`only` filter for the golden and determinism tests; `cargo clippy --target
aarch64-unknown-linux-gnu` (needs only `rustup target add`), plus the golden
under qemu-user if available; `#[cfg(all(target_arch = "x86",
not(target_feature = "sse2")))] compile_error!(...)` in `crmath/mod.rs`. None of
these covers aarch64 execution without a runner.

Enforceable: yes, as `[[check]]` entries and a `compile_error!`.

## VD-008 - "Caches never change cost or behaviour" is checked on a handful of sources

Reported by: compiler-cost, snapshot.

`docs/determinism.md` "Inline caches": whether an instruction succeeds, what it
returns and costs, its stack need and metamethod depth "never depend on which
values the site saw before", and a State resumed from a snapshot "continues
exactly as the State that saved it would have". The evidence is hand-written
cases (`bytecode_caches.rs`, `global_write_cache.rs`, `plain_field_miss.rs`;
gmatch charges in `tests/integration/cost/cost_determinism.rs`; output and bytes in a handful of
save tests). No test compares `cost_used()` after a continuation through cold
caches against a continuous run. (The one test that claims warm-across-executions
runs cold-versus-cold; see verif-assertions.md.)

Fix: a test-infrastructure feature that disables every inline cache (always take
the miss path), and a differential test running the corpus with and without it,
requiring identical output, `cost_used` and error. That proves the claim over the
corpus and would catch cache-key aliasing bugs. The resume half is held by the
corpus save round trip (VD-009).

Enforceable: yes, by the differential test.

## VD-009 - No corpus-wide save round trip

Reported by: snapshot.

All save coverage comes from hand-built graphs in the snapshot suites plus about
15 scattered `cfg(snapshot)` tests. The Lua examples, which exercise most of the
VM's object shapes (stress tables, closures, upvalue scopes, metatable edge
cases, gmatch, sort, queues), never pass through `save_state`. A save bug in a
shape nobody hand-wrote is caught only if a game hits it.

Fix: under `snapshot`, a corpus test or a `run_examples` mode through a CLI
`--save-roundtrip` flag that exists only with the feature. Per example: run the
top level; `save_state`, `load_state`, re-save and assert identical bytes; if it
defines `_bench`, call it on both the original and the loaded State; compare
outputs, `cost_used()` and a final save. That holds the "continues exactly"
claim (VD-008), the byte-stability claim and the verifier's acceptance of
everything the compiler emits (`compiler_saves_contain_no_closure_free_closes` is
the one-chunk version today), gives the corruption sweep a real corpus to mutate,
and makes the second (snapshot) sweep earn its cost.

Enforceable: yes; the corpus walker is shared, so new examples are covered
automatically.

## VD-012 - The gmatch schema is a persisted format with no golden, and its recognition fails open

Reported by: library, snapshot.

`gmatch_schema` is shared by the stdlib registration and the verifier, so the two
cannot drift by spelling. But renaming `"pos"` or `"string.gmatch.iter"` passes
every round-trip test (save and load use the same build) while breaking every
snapshot saved by an older build; the save golden program has no gmatch closure.

The verifier recognises a gmatch iterator closure by its upvalue holding
`SavedVal::Fn(gmatch_schema::ITERATOR_ID or EMPTY_ITERATOR_ID)`
(`VerifiedSavePayload::gmatch_iterator_chunks`, `validate_gmatch_iterators`). If
the stdlib stops registering the iterator under that constant (it becomes a
different Rust function, or the wrapper stops capturing it as an upvalue), no
saved closure matches, `validate_gmatch_iterators` silently validates nothing,
and the forged-state checks it exists for go dark with no error.

Fix: add a live gmatch iterator, empty and nonempty, to
`tests/common/save_golden_program.rs`; a test that saves a live iterator and
asserts `gmatch_iterator_chunks()` is non-empty. Structurally: give the gmatch
iterator its own `SavedObject` variant so the verifier reaches it by type, not by
name.

Enforceable: the golden and the test; the dedicated variant makes it structural.

## VD-013 - `GOLDEN_PROGRAM` does not exercise "every edge kind"

Reported by: snapshot.

Its doc says it "exercises every edge kind the walker can take". It has no sealed
globals (so no pins), no tombstones, no non-string global keys, no two closures
sharing one upvalue cell, no gmatch iterator, no environment-delta array span or
hash slots, no metatable delta, and no rendered `Fn` identity. False today.

Fix: decode the golden and assert each `SavedVal`, `SavedObject` and
payload-section variant appears at least once, using an exhaustive match so a new
variant fails to compile until the golden covers it.

Enforceable: yes, by the exhaustive match.

## VD-018 - The host's floating-point environment is assumed, never detected

Reported by: library.

The library assumes round-to-nearest and no flush-to-zero or
denormals-are-zero: `reference/deterministic-math.md` names it the supported
environment, and crmath's correctness, `numeral`'s subnormal parsing and every
arithmetic opcode depend on it. A game engine embedding dellingr is exactly the
kind of host that sets FTZ/DAZ in MXCSR, or changes the rounding mode, on its
threads for speed. Every subnormal-touching result, and possibly kernel outputs,
would then differ between hosts with no error: a silent replay break. The tests
run in the default environment and cannot see it.

Fix: a cheap probe at State creation and each `call` entry (e.g.
`black_box(f64::MIN_POSITIVE) / black_box(2.0) != 0.0` detects FTZ; a
subnormal-times-one probe detects DAZ) returning an error or panicking in debug;
a unit test that sets MXCSR FTZ on x86 and expects the refusal.

Enforceable: by the probe plus that test.

## VD-019 - `%` and `math.fmod` lower to the platform `fmod` with no edge-case golden

Reported by: library.

`docs/determinism.md` states this correctly, and IEEE `fmod` is exact, so correct
libms agree. There is still no golden of remainder edge cases (signed zeros,
subnormal divisors, huge quotients) to show it on a given host. The libm symbol
check in verif-gates.md would at least record `fmod` as the one permitted libm
import.

Enforceable: yes, by a golden.
