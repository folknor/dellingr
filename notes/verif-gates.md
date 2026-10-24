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

A `fixtures/lint-policy/` fixture now proves every method and type ban fires,
`disallowed_types` is deny, the f32 list is complete, a dependency rule
replaces the unresolvable `rand` bans, and a libm symbol check reads the
release binaries. Remaining:

- `allow-invalid = true` stays on the four foldhash `RandomState` entries until
  the fixture has run and proven they resolve; drop it then.
- Host state still unbanned: `std::env::current_dir`, `home_dir`,
  `std::process::id`, `std::thread::available_parallelism` and pointer-address
  values (`temp_dir` and `std::thread::current` are now banned). Banning
  them needs scoped allows in `examples/regenerate_save_golden.rs`, the test
  scratch and support code, the crmath sweeps and the native-stack probe.
- The libm symbol check is gated only on the dev CLI binaries
  (`corpus::libm_symbols`); the shipped release binaries need the manual
  `scripts/check_libm_symbols.sh`, because a script check cannot get built
  artifacts inside `brokkr check` (brokkr#20). It also sees linked CLI code
  only, not library code the linker discarded.

Fix: drop foldhash's `allow-invalid` once the fixture proves it; add the
host-state bans with their scoped allows in one wave.

Enforceable: yes, by the fixture.

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

## VG-017 - Nothing checks the consumer's dependency graph stays free of the profiler's crates

Reported by: check-machinery.

A `dependency_rule` now allowlists the direct normal and build dependencies.
What remains is transitive: `hotpath`'s own dependencies (`tiny_http`,
`quanta`, `time`, `serde_json`, `crossbeam-channel`, `hdrhistogram`) are not
checked against the consumer's resolved default-feature graph. brokkr's
`dependency_rule` sees direct edges only, and `brokkr deps` cannot run nested
inside `brokkr check` (brokkr#21).

Fix: a transitive dependency rule once brokkr offers one.

Enforceable: blocked on brokkr#21.

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
