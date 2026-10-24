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

## VG-009 - Source-only checks compile and run as integration tests in every sweep

Reported by: check-machinery.

These inspect files only, yet are compiled and run once per dev sweep: every
test in
`corpus::test_conventions` (more of them since the integration merge),
`diff_test.rs::diff_markers_have_nonempty_reasons`,
`diff_test.rs::shell_and_rust_example_corpora_match`, and
`diff_test.rs::malformed_timeout_is_rejected...`. They fail late, after clippy
and a full build.

Fix: textlint rules, or one pre-clippy `script_check`, for the marker-reason
rule and the conventions. The corpus-equivalence test exists only because two
walkers exist; the in-process gate rewrite in rewrites.md removes it.

Enforceable: yes.

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
sweep with `tests = [...]`, `lib_tests`, or `skip`/`only` filters. (brokkr's
`exclude_tests` drops whole test targets and only on lanes that run their
binaries themselves, so it cannot drop subjects inside the integration crate.)

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
