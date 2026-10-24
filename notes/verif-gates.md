# Verification: gates and check machinery

Findings from the verification hunt about the check machinery itself:
`tests/integration/corpus/test_conventions.rs`, `clippy.toml`, textlint and script checks,
`scripts/native_calls.py`, the gremlins gate, feature shapes and sweeps in
`brokkr.toml`, the `Cargo.toml` package, generators, and the scripts around
them. Each entry names the scopes whose hunters reported it.

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

## VG-001 - `test_conventions.rs` checks words, misses spellings, over-reaches, and sees only `tests/`

Reported by: check-machinery, corpus-harness, native-stack-errors, runtime,
compiler-cost, library.

`cli_paths_and_test_file_mutations_stay_in_support_helpers` flags file
mutations outside `tests/integration/support/scratch.rs` by substring. The
merged crate's newer convention tests (`every_integration_source_is_reached_from_main`,
`process_global_constructs_are_marked`, ...) share its lexical approach.
Problems:

- Missed spellings: `fs::remove_dir_all(` (`fs::remove_dir(` does not match
  it), `fs::rename(`, `fs::copy(`, `File::options(`, `fs::hard_link(`,
  `fs::set_permissions(`, `std::os::unix::fs::symlink(`, and an imported bare
  `write(` after `use std::fs::write`. Writes by spawned processes (the diff
  gate's `mktemp` into `.scratch/`) are invisible to it.
- Its final assertion checks that `scratch.rs` *contains the strings*
  `CARGO_TARGET_TMPDIR` and `.starts_with(scratch_root)`; a comment carrying
  both would satisfy it. The guard it means to prove (test mutations stay
  under Cargo's temporary directory) is already false for the file it exempts:
  `scratch.rs::write_boundary_transcript` does `fs::create_dir_all` and
  `fs::write` into `$CARGO_MANIFEST_DIR/.scratch/error-boundary/`, bypassing
  `write_scratch_file`'s root check. Its doc comment admits this, but the
  failure message ("test file mutations must be restricted to Cargo's
  temporary test directory") states otherwise. That doc comment also calls the
  location "spec-mandated"; no spec in `reference/` or `docs/` mentions it, so
  a durable comment cites a transient document.
- Over-reach: `write_all(` is banned everywhere outside `scratch.rs`,
  including writes into a child's stdin pipe, so
  `tests/integration/cli/cli_arguments.rs::run_with_stdin` writes its stdin bytes to a scratch
  *file* and opens it as stdin instead of piping in memory. The lexical rule
  caused the filesystem writes it was meant to prevent.
- It scans only `tests/`. In-crate unit tests under `src/` are outside it:
  `src/crmath/tests/golden.rs` does `std::fs::write(GOLDEN_PATH, ...)` into the
  tracked fixture, spawns `python3` with `Command::new`, and reads a gitignored
  sibling checkout. The "no CLI binary path outside support" rule cannot see
  `src/` either. `examples/` (the save-golden regenerator's `fs::write`) is
  also outside it, deliberately.
- The AGENTS.md rule "no test may reference `target/` or spawn cargo" has no
  check at all.
- It is compiled and run once per sweep, after clippy and a full build.

Fixes proposed (hunters differ on the mechanism):

- check-machinery: move it to brokkr `[[textlint]]` rules with
  `test_code = "only"` over `src/**/*.rs` and `tests/**/*.rs` (brokkr's
  classifier finds test code inside `src/`), `region = "code"`, with narrower
  patterns (real filesystem mutators, `Command::new("cargo")`, `"target/` in
  `region = "string"`) and `must_scan` sentinels. Textlint runs pre-clippy,
  costs no compile and fails fast. Replace the string assertion with a unit
  test of the guard: call `write_scratch_file` with a path outside the root and
  assert `PermissionDenied`.
- corpus-harness and runtime: clippy `disallowed_methods` for the `std::fs`
  mutators, `File::create`, `OpenOptions::new` and
  `std::process::Command::new`, with a scoped
  `#![allow(clippy::disallowed_methods)]` in `scratch.rs` and `cli.rs`. Clippy
  resolves paths, so imports and aliases cannot dodge it, and it covers `src/`
  test modules; the cost is scoped allows in `src/` and `examples/`.
- native-stack-errors: require that the only mutating calls in `scratch.rs` are
  inside `write_scratch_file` and `ScratchDirectory` (a function-level scan, or
  move the mutation primitives behind one private function and count them).

All agree the transcript writer should leave `scratch.rs` or get a named
exemption (see the `.scratch` entry in verif-corpus.md).

Enforceable: yes, by any of the three.

## VG-002 - `clippy.toml` bans fail open, and nothing proves any of them fires

Reported by: check-machinery, compiler-cost, library.

- `allow-invalid = true` on `rand::rng`, `rand::thread_rng` and the four
  `foldhash::{fast,quality}::{RandomState,SeedableRandomState}` entries. With
  `rand` not a dependency, the first two cannot resolve and seal nothing; a
  re-introduced `rand` would come in through `rand::random`,
  `SeedableRng::from_os_rng`/`from_entropy` or `getrandom`, none of which is
  listed. Whether the foldhash paths resolve depends on foldhash's features,
  which an embedding host can change through unification. `allow-invalid`
  hides both outcomes.
- brokkr has a `[[fixture_lint]]` phase built for exactly this ("Those lints
  fail open: a configured path that no longer resolves ... seals nothing",
  `brokkr man conventions fixture_lint`). Dellingr does not use it (the check
  log says `fixture-lint: not applicable`). No entry is proven to fire,
  including the determinism-critical `f64::sin` family, `Instant::now` and
  `HashMap`.
- Host state that is not banned: `std::env::current_dir`, `temp_dir`,
  `home_dir`, `std::process::id`, `std::thread::current().id()`,
  `std::thread::available_parallelism`, and pointer-address values
  (`<*const T>::addr`, `expose_provenance`). No use in `src/` today; a missing
  guard, not a live bug.
- `Result::unwrap_or` and `unwrap_or_default` are banned as error-discarding,
  but `Result::unwrap_or_else(|_| ..)`, `map_or` and `map_or_else` discard the
  error the same way and are allowed.
- `[lints.clippy]` sets `disallowed_methods = "deny"` but not
  `disallowed_types`, which defaults to warn. Inside brokkr every diagnostic
  fails, so the gate is unaffected; a plain `cargo build`, rust-analyzer or a
  consumer sees a banned `HashMap` as a warning and a banned `sin` as an error.
- The libm ban is a name list. The f64 list is thorough; the f32 list lacks
  `sinh` through `atanh`, `exp2`, `exp_m1`, `log2`, `log`, `ln_1p`, `cbrt`,
  `hypot` and `sin_cos`. Any libm-backed method std stabilizes later
  (`gamma`/`ln_gamma`/`erf`/`erfc`, unstable now) is not listed. The claim in
  `reference/deterministic-math.md`, "nothing in the library calls the
  platform libm", cannot be enforced by a name list.

Fix: add a `[[fixture_lint]]` crate that calls every banned path, with an
`expect.toml`; drop `allow-invalid` wherever the fixture proves the path
resolves; add the missing host-state paths and the error-discarding
combinators; make `disallowed_types = "deny"`. For libm (library): a
`[[script_check]]` on the release artifact listing undefined libm symbols
(`nm --undefined-only`, or `objdump -d` for calls to `sin|cos|tan|exp|log|pow|
cbrt|hypot|...`), allowing only `fmod`. That also catches LLVM lowering an
intrinsic to a libcall, and dependencies doing so; it needs the toolchain's
`nm` or `objdump`, which `scripts/frame_sizes.py` already assumes.

Enforceable: yes. That is what `fixture_lint` exists for; the symbol check is
mechanical.

## VG-003 - Three features no gate compiles

Reported by: check-machinery, native-stack-errors, runtime, library.

`hotpath`, `hotpath-alloc` and `fault-injection` appear in no `[[check]]`.
Uncompiled and unlinted as a result:

- every `#[hotpath::measure]` expansion, and the `cfg(feature = "hotpath")`
  branches in `tests/common/stack.rs` and `tests/integration/corpus/diff_test.rs`;
- `src/vm/fault_injection.rs`, the six `cfg(feature = "fault-injection")` arms
  in `eval.rs`, `stack.rs` and `metamethod.rs`, the debug assertion in
  `vm.rs::validate_quiescent` (`debug_assert_fault_injection_disarmed` on the
  snapshot path), the unit tests in `fault_injection.rs` (two) and
  `src/vm/registers.rs` (`cfg(all(test, feature = "fault-injection"))`);
- the *non-ignored*
  `string_bytes.rs::gmatch_refused_push_preserves_the_next_match`, the only
  fault-injection test outside `error_boundary.rs`. The documented on-demand
  command (`brokkr check --features fault-injection,snapshot -- --test
  integration -- failures::error_boundary:: --include-ignored`) filters to the
  `error_boundary` module, so it is reached only by an undocumented run of the
  whole integration crate under the feature.

AGENTS.md asks a human to remember `brokkr check --features hotpath`. A bad
annotation surfaces on the next `--hotpath` run; a rename in any preflight
breaks fault injection silently until someone runs the on-demand sweep. The
fault sweep is the only test of "every eligible refusal leaves the operands,
notifications and quiescence right", the error-cleanup contract AGENTS.md calls
load-bearing.

Fix options:
- a `background` `script_check` running
  `cargo clippy --all-targets --features hotpath,hotpath-alloc,fault-injection`
  into its own target dir (the manual endorses this for "one that drives its own
  cargo");
- a `[[check]]` shape with `features = ["fault-injection", "snapshot"]`,
  `tests = ["integration"]`, a filter to `failures::error_boundary::` and
  `library::string_bytes::` if a `[[check]]` can carry one (unconfirmed), and
  `include_ignored`, plus the lib unit tests if
  brokkr can select them (see brokkr.md); cost estimated as one more dev build
  of the library plus the integration crate, unmeasured; at
  minimum a clippy-only lane;
- a `curated` `[[check]]` entry with those features and a narrow `only` filter;
- for `string_bytes`: move the test into `error_boundary.rs`, or change the
  documented filter to include `string_bytes`, with a convention check that
  `feature = "fault-injection"` appears only in files the documented command
  reaches.

Enforceable: yes, in config.

## VG-004 - Test-only fault-injection API is reachable by any host through feature unification

Reported by: native-stack-errors, runtime.

`fault-injection` is an ordinary Cargo feature, and `arm_fault_injection` /
`finish_fault_injection` are `pub` (`#[doc(hidden)]`). Any crate in an
embedder's graph that enables `dellingr/fault-injection` turns them on in the
game's VM, along with a branch on every stack and depth preflight. AGENTS.md
says it "is test infrastructure, never a production ... configuration", but
nothing stops it.

Fix options: gate it on a rustc cfg (`--cfg dellingr_fault_injection`, set by a
brokkr sweep's RUSTFLAGS) so dependency features cannot reach it (native); add
`#[cfg(all(feature = "fault-injection", not(debug_assertions)))]
compile_error!(...)` since the documented sweeps are all dev builds (runtime);
or keep the feature with a `compile_error!` unless a second explicit opt-in cfg
is present; or rename it to announce itself (`__test-fault-injection`).

Enforceable: yes, structurally or by `compile_error!`.

## VG-005 - `native_calls.py` skips files by name pattern

Reported by: check-machinery.

`production_sources` drops any `src/` file whose path matches
`(^|/)(tests?|[a-z_]*_tests)(\.rs|/)`, without checking that its `mod`
declaration is `#[cfg(test)]`. Today every such file is (`vm/tests.rs`,
`compiler/parser/tests.rs`, `line_attribution_tests.rs`,
`save_state/integrity_tests.rs`, `crmath/tests/`). A production module named
`test.rs` or `foo_tests.rs` would be silently unscanned, and its calls into Lua
would need no markers.

Fix: for every file the name rule skips, find the `mod` item that declares it
and require a `cfg(test)` or `cfg(all(test, ..))` attribute, or report it.

Enforceable: yes, in the script.

## VG-006 - Indirect calls into Lua are a reviewer-only rule

Reported by: check-machinery.

`reference/native-calls.md`: "A new indirect call that can reach Lua needs this
guard extended before it lands." Nothing detects a new indirect path. Storing
`State::call_unguarded` in a variable, passing an inventoried function as a
`fn` value, or a new `dyn` trait whose implementation re-enters: none is a
lexical call, so neither the script nor the textlint rule sees it.

Fix (partial): a textlint rule flagging any inventoried name used as a value
(followed by neither `(` nor `::<`), with the same generated alternation;
`native_calls.py` can verify that rule's pattern as it already does for
`native-call-marker`. Trait-object paths stay a review item.

Enforceable: partly.

## VG-007 - `native_calls.py` has no tests, and its alternation includes very common names

Reported by: native-stack-errors, check-machinery.

- The script's lexer (raw strings, char literals versus lifetimes, nested
  block comments, closure callbacks spelled exactly `|state|`) is what every
  marker and coverage claim rests on. A lexer regression that hides a new call
  site passes silently; one that hides an existing site fails loudly. Unlike
  `notes_sections.py` (tested by `test_notes_sections.py` as a script check),
  it has no tests.
- The textlint alternation includes `call`, `run`, `format`, `main`,
  `get_table`, `set_table`. Any new method with one of those names on an
  unrelated type becomes a "stray" the script asks to exclude by file. That
  pushes toward excluding whole files, which is what `src/compiler/**` already
  is. Receiver-aware matching (already modelled in the script) would let the
  script, not the regex, decide.

Fix: a fixture-based `test_native_calls.py` in the same `script_check` pattern;
receiver-aware stray decisions.

Enforceable: yes, by the script test.

## VG-008 - The convention phase costs about 9 seconds per check and its script grows superlinearly

Reported by: check-machinery, native-stack-errors.

Both check logs report "gremlins (219 files), textlint (1 rule, 79 files),
script-check (2 checks), publish cycle: ok in 9.1s", against 406ms for warm
clippy and 1m10s for the whole warm check. The split of the 9.1s was not
measured. `native_calls.py` is the main suspect:

- `stray_excluded` calls `textlint_excludes()`, which re-reads and re-parses
  `brokkr.toml` for each of about 100 marked sites and again per stray
  candidate;
- `attach_markers` does `s.code.split("\n")` once per comment, quadratic in
  file size;
- `textlint_strays` re-splits the file and sums line lengths per matching line,
  also quadratic, and the common-name alternation (VG-007) means many matching
  lines;
- `closure()` rescans every call token in `src/` per reached key;
  `enclosing()` and `in_test()` are linear scans per call;
- `scan_test_lists` re-sanitizes the whole test file once per list (three
  times).

`src/vm/eval.rs`, `frame.rs` and `table_ops.rs` grow with the VM, so the cost
rises faster than the tree. The script also imports `tomllib` without a
version check, so on Python older than 3.11 it raises an ImportError traceback;
`scripts/lib.sh::workload_hotpath_files` checks the version properly.

Separately, `generated-large-source` runs at `stage = "post-test"` although it
is a pure source comparison, so it is skipped whenever any test fails and
reports last.

Fix: index once per file (line starts, function spans sorted for bisection,
call tokens grouped by name, parsed config passed in); add the Python version
guard; move the generated-file check to `pre-clippy`.

Enforceable: a script check's wall time is visible in the run log; no ceiling
other than brokkr's 5-minute `script_check` limit applies.

## VG-009 - Source-only checks compile and run as integration tests in every sweep

Reported by: check-machinery.

These inspect files only, yet are compiled and run once per sweep (and under
the release sweeps whenever those widen, see brokkr.md): every test in
`corpus::test_conventions` (more of them since the integration merge),
`diff_test.rs::diff_markers_have_nonempty_reasons`,
`diff_test.rs::shell_and_rust_example_corpora_match`, and
`diff_test.rs::malformed_timeout_is_rejected...`. They fail late, after clippy
and a full build.

Fix: textlint rules, or one pre-clippy `script_check`, for the marker-reason
rule and the conventions. The corpus-equivalence test exists only because two
walkers exist; the in-process gate rewrite in rewrites.md removes it.

Enforceable: yes.

## VG-010 - Generators and regenerators rewrite identical bytes

Reported by: check-machinery, corpus-harness, library, snapshot.

- `scripts/crmath_tables.py --generate` calls `destination.write_bytes(output)`
  for every table file unconditionally. A no-op regeneration touches eleven
  `src/` files, so cargo recompiles the crate and every test binary in every
  shape.
- `examples/parse/generate.py` always rewrites `large_source.lua`, churning
  brokkr's mtime-cached hash.
- `regenerate_crmath_golden` rewrites its fixture unconditionally (see also
  bugs.md "A focused brokkr test runs ignored tests that rewrite a tracked
  fixture").
- `examples/regenerate_save_golden.rs` writes unconditionally (matters little
  for a manual tool).
- `crmath_tables.py`'s mismatch path calls `destination.read_text()` with no
  encoding, so it uses the locale default; harmless for ASCII tables,
  `encoding="utf-8"` would make it explicit.

(`scripts/notes_move.py`'s unconditional rewrites are filed with its data-loss
bug in bugs.md.)

Fix: write only when the bytes differ, using one shared helper; say so when
skipping.

Enforceable: partly. A textlint rule over `scripts/**` and `examples/**/*.py`
can ban `.write_text(`/`.write_bytes(`/`open(..., "w")` outside the helper.

## VG-011 - The CORE-MATH pin is written in five places, and the generated tables' "do not edit by hand" is unenforced

Reported by: check-machinery, library.

The sha `0ce0cbc1...` appears in `scripts/crmath_tables.py`
(`CORE_MATH_COMMIT`), `src/crmath/tests/golden.rs` (`CORE_MATH_COMMIT`),
`src/crmath/LICENSE-CORE-MATH`, `reference/deterministic-math.md` and the
golden fixture header. Only the golden.rs constant is enforced, and only by an
ignored corpus pin. No file records where `research/core-math` comes from
(no clone URL; only `research/*/` in `.gitignore`).

`reference/deterministic-math.md` says the tables "are not edited by hand" and
tells the reader to run `--check`. `crmath_tables.py --check` runs only via the
ignored `core_math_tables_match_pin`, which needs `python3` on PATH and
`research/core-math` at the pinned commit. Nothing in `brokkr check` notices a
hand edit to `src/crmath/tables/*.rs` unless it changes a golden row. Every
sweep, the regenerator, the corpus pin and the table check resolve
`CARGO_MANIFEST_DIR/research/core-math`, a gitignored sibling at a fixed
layout; they fail loudly when it is missing.

Fix options: single-source the pin, with the URL, in a small TOML both read,
and record each generated table file's digest there, checked by a pre-clippy
`script_check` that needs no checkout (check-machinery); or vendor the ~15
MIT-licensed table-bearing upstream C files unmodified under
`reference/core-math/` or `src/crmath/upstream/` and make `--check` a
`[[script_check]]` with no network and no checkout (library); and a
non-ignored unit test that `include_str!`s the script, the licence and the
fixture and asserts each contains `CORE_MATH_COMMIT`, with the reference doc
pointing at the constant instead of repeating the sha. The worst-case corpus
(several million lines) should stay external; its readers are already pinned
by count and digest.

Enforceable: yes.

## VG-012 - `[gremlins] exclude` exempts a whole file

Reported by: check-machinery, library.

`brokkr.toml` has `exclude = ["tests/integration/library/string_bytes.rs"]`. The manual says
"Entries are project-root-relative directories matched by path prefix", so
whether a file path matches is behaviour the manual does not promise. The
file's non-ASCII content is six lines: a literal U+26A0 in Lua source and a
U+FFFD expectation in Rust. The whole-file exemption would hide any real
gremlin added there later.

Fix: write the Lua as `"\xE2\x9A\xA0"` (or `"\226\154\160"`) and the Rust as
`"\u{fffd}a"`, then drop the exclusion. `brokkr check --audit` reports an
exclusion that excuses nothing (not run by the hunter).

Enforceable: yes, by deleting the exemption; a convention check could reject a
non-empty `[gremlins] exclude`.

## VG-013 - The published package carries tests that cannot run, and ships files they do not need

Reported by: check-machinery, snapshot.

`Cargo.toml`'s comment says `tests/` and `examples/` stay in the package
because they carry the corpus `run_examples.rs` and the diff gate execute. Yet
the package excludes `scripts/` (which holds `diff_test.sh`, driven by
`tests/integration/corpus/diff_test.rs`, and `test_limited.sh`, driven by
`tests/integration/support/process.rs::limited_command`), `bench/` and
`brokkr.toml` (both read by `tests/integration/corpus/bench_scripts.rs`).
`cargo test` on the published crate therefore fails in at least three suites; `cargo package` verifies only that the crate
builds. Meanwhile keeping `tests/` ships `tests/fixtures/crmath_golden.txt`
(557 KB) and the save golden, while the `.lua` corpus the comment cites lives
in `examples/`.

Fix: publish a coherent test surface (an `include` allowlist naming what the
tests read) or none. Excluding `tests/` and the `.lua` corpus keeps
`examples/hotpath.rs` and `regenerate_save_golden.rs`, which are all the
declared `[[example]]` targets need.

Enforceable: yes. A background `script_check` running `cargo package --list`
asserting every path the tests open is in it, or that `tests/` is not.

## VG-014 - Doctests never run, and there is no rustdoc phase

Reported by: check-machinery.

`[test] doctests` is unset, which brokkr defaults to `false`; the check log
shows every sweep scoped to `--tests`. The five doc examples (crate docs in
`src/lib.rs` and `src/host.rs`, `Program`'s in `src/lib.rs`, and two in
`src/vm.rs`: `with_callbacks`, `set_user_data`) are the consumer-facing API
surface, and none is compiled. There is no `[rustdoc]` table, so broken
intra-doc links go unnoticed.

Fix: `doctests = true` (dev sweeps only) or a `doc_only` entry, plus a
`[rustdoc]` table.

Enforceable: yes, in config.

## VG-015 - Durable comments cite tracker IDs whose documents are gone

Reported by: runtime, library, native-stack-errors.

No file in the repository defines these IDs (not `notes/`, `docs/`,
`reference/` or `CHANGELOG.md`):

- `tests/integration/host/host_table_api_atomicity.rs` module doc: `VM-006`,
  `VM-007`, `VM-025`; `src/vm/table_ops.rs` and
  `tests/integration/library/stdlib_divergences.rs` cite
  others;
- `stdlib_divergences.rs`: `VM-041`, `STD-014`, `STD-015`;
  `stdlib_numeric_format.rs`: `STD-004`, `STD-007`, `STD-009`;
  `number_format_g14.rs`: `STD-005`; `string_format.rs`: `C30`;
- `error_handling.rs::table_sort_charges_before_mutating` cites "(L18)";
  `rustfn_error.rs` cites "(L6)" twice.

They pointed at transient documents. AGENTS.md requires a code comment to carry
its own context. The module docs mostly already state the properties, so the
ids can simply go.

Fix: replace each id with the property it names.

Enforceable: yes, a `[[textlint]]` banning `\b(STD|VM)-\d{3}\b`,
`\bC\d{2}\b:` and `\(L\d+\)` in `src/**` and `tests/**`.

## VG-016 - No `unsafe` policy lint

Reported by: check-machinery.

There is `unsafe impl Sync for RuntimeCaches` (`src/vm/caches.rs`) and about 14
`unsafe` calls into the hardware FMA kernels in `src/crmath/`. No
`unsafe_code` level and no `clippy::undocumented_unsafe_blocks` is set, so a
new `unsafe` needs neither a justification nor a review flag.

Enforceable: yes, by lint level.

## VG-017 - Nothing checks the consumer's dependency graph stays free of the profiler's crates

Reported by: check-machinery.

`hotpath` is a non-optional dependency of an embeddable deterministic library.
`Cargo.lock` lists `tiny_http`, `quanta`, `time`, `serde_json`,
`crossbeam-channel` and `hdrhistogram` among its possible dependencies; the
hunter did not verify which compile with the feature off. The `Cargo.toml`
comment says it "costs only compile time", but nothing checks that the
consumer's graph stays free of a timing or HTTP crate.

Fix: a `cargo tree -e normal` script check against an allowlist.

Enforceable: yes.

## VG-018 - The default and snapshot sweeps share one `CARGO_BIN_EXE_dellingr` path

Reported by: check-machinery, corpus-harness, snapshot.

Both check logs show, for every sweep on every run: "prepare: sweep 'default':
rebuilt before listing - /media/folk/Banan/cargo/debug/dellingr has other
content", and the same for `snapshot`, and for the two release sweeps
(`.../release/dellingr has other content`). Each feature shape's `cargo test
--no-run` re-uplifts `target/<profile>/dellingr`, so the binary
`CARGO_BIN_EXE_dellingr` names belongs to whichever shape built last.

`tests/integration/corpus/diff_test.rs`'s claim that "the corpus is checked against the CLI build
of that shape" holds only because brokkr runs sweeps serially and re-uplifts
between them, and because nothing else writes that path mid-run. Under plain
`cargo test`, a parallel sweep layout, a `brokkr run --debug` between prepare
and run, an editor-driven build, or a `brokkr test` in one shape during a check
in the other, a snapshot test can drive the default CLI (or the reverse) and
pass. Cost today: a relink per sweep on every check.

Fix options: give the `snapshot` sweeps their own target dir (`[[check]] env =
{ CARGO_TARGET_DIR = ... }`; the manual forbids it only alongside `rustflags`),
which costs disk and removes the re-uplift (check-machinery, snapshot); have
tests find the binary through the deps-hashed artifact cargo reports rather
than the uplifted path (snapshot); have the CLI report its feature shape
(`dellingr --build-info`, or a line in `--help`) and have `run_examples` and
`diff_test` assert it matches `cfg!(feature = "snapshot")` before running
anything (corpus-harness).

Enforceable: yes, in config or by that assertion.

## VG-019 - The snapshot sweep repeats the default sweep and adds little

Reported by: snapshot, compiler-cost.

The last check log shows 968 tests in `default` and 1145 in `snapshot`. The
snapshot sweep re-runs the whole default suite, including `run_examples` and
the diff gate (with the reference Luas re-run, whose output cannot depend on
dellingr's features), and adds about 177 snapshot tests. The `brokkr.toml`
comment justifies the sweep by the `cfg(not(feature = "snapshot"))` sides
(stdlib registration, `_G` metatable, RNG naming), which is true for clippy and
compile; for the test run it means 968 tests whose behaviour mostly cannot
differ. Nothing in `src/compiler/` except `verify.rs` has a `snapshot` cfg, so
every parser, lexer, codegen and depth test runs identically twice. The
duplicate run of the cost goldens is useful (cost does not depend on the
feature, which changes stdlib registration), but only by accident. The test
phase was 55.2s across four sweeps; the snapshot share is plausibly close to
half (not in the log).

Fix: make the second run earn its cost (the corpus save round trip in
verif-goldens.md; an explicit cross-shape cost equality); run the reference-Lua
half of the diff gate once per check, not per shape; restrict the snapshot
sweep with `tests = [...]` or an exclusion if brokkr has one (see brokkr.md).

Enforceable: yes, in `brokkr.toml`.

## VG-020 - The rules forbid the benchmarking tools the docs prescribe

Reported by: check-machinery.

The CLAUDE.md benchmarking rules say "Never hyperfine, `scripts/bench.sh`,
`scripts/hotbench.sh`". AGENTS.md documents both scripts as the tools, and
`brokkr man dellingr` says comparison against reference Lua is "not part of
this surface - `scripts/bench.sh` in the dellingr repo owns that". The README
ratio table and the `lua-rs` column (via `research/bench_luars.sh`) can
therefore only be produced by a tool the rules forbid.

`hotbench.sh` is fully superseded by `brokkr dellingr --lua W --hotpath`; its
hyperfine half times the ms-scale `hotpath_file`, which the same rules say is
below launch noise. `scripts/diff_gate.sh` is invoked by nothing, and `brokkr
test differential_examples --debug` covers its job.

Fix: delete `hotbench.sh` and `diff_gate.sh`. Decide whether vs-Lua ratios are a
measurement the project still publishes; if so the rule needs an explicit
carve-out, and the tool needs the same provenance discipline (hostname,
commit) as `brokkr results`.

Enforceable: partly. A deleted script cannot drift.

## VG-021 - `research/bench_luars.sh` has several faults

Reported by: check-machinery.

- It runs whatever `target/release/dellingr` holds (see brokkr.md, the
  artifact-path entry), and hard-codes `DELLINGR="target/release/dellingr"`,
  which makes AGENTS.md's "no script hard-codes a `target/` path" false.
- It writes `.bench-luars.json` at the repo root, which is not in
  `.gitignore`, so an interrupted run leaves an untracked file; it should use
  `.scratch/bench-luars/`.
- It does not anchor to the repo root.
- It hard-codes `--warmup 2` instead of `HYPERFINE_WARMUP`.
- Its header says it "Mirrors ../bench.sh's jq extraction", but `bench.sh`
  lives in `scripts/`.
- It tells the reader to `cargo build --release`, which the project rules ban.

Enforceable: partly, by the textlint rule in brokkr.md's artifact-path entry.

## VG-022 - Smaller script and config drift

Reported by: check-machinery.

- `scripts/ab_pairs.py` is undocumented. AGENTS.md describes every other helper
  script but not this one, which implements the CLAUDE.md "interleave matched
  A/B cells" rule.
- Two lists of reference interpreters: `scripts/lib.sh` has `REFERENCE_LUAS`
  (5.2, 5.4) and `scripts/bench.sh` its own (5.5, 5.4, 5.2, luajit).
- `.review.toml` holds only `[_audit] id = "40c9"`, has no comment naming its
  consumer, and is excluded from the package. If the `review` tool owns it, a
  one-line comment saying so would tell a reader whether it can be edited.

Enforceable: no, beyond deduplicating the list.

## VG-023 - "The only environment input" claims are false as stated

Reported by: corpus-harness.

`scripts/lib.sh` and AGENTS.md say `BROKKR_MARKER_FIFO` is "the only
environment input in the repository" and that "the scripts read no environment
variables". The harness also consumes `HOTPATH_OUTPUT_PATH` (through the
`hotpath` crate; brokkr sets it); every script resolves `bash`, `lua5.2`,
`lua5.4`, `timeout`, `python3` and `cargo` through `PATH`; and `cargo` itself
reads `~/.cargo/config.toml` (see verif-native-stack.md on what that config
changes).

Fix: reword to "reads no environment variables of its own" and list what is
resolved through `PATH`.

Enforceable: no.
