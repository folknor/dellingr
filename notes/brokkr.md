# brokkr-side findings

Findings from the verification hunt whose fix lies in brokkr's behaviour
rather than in dellingr's code, or where dellingr's workaround depends on what
brokkr offers. Each entry names the scopes whose hunters reported it and, where
a hunter proposed one, the dellingr-side workaround.

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

## BRK-001 - A focused `brokkr test` builds the whole suite in release, twice

Reported by: check-machinery, native-stack-errors, snapshot.

Evidence: `.brokkr/check-logs/test-001792800558168.log`
(`brokkr test a_site_holding_two_classes`, plantasjen). It shows four sweeps.
For `release-native-stack` and `release-native-stack-snapshot` it built
`--release -p dellingr --tests`, all 80 harnesses rather than only
`native_stack`, in 54.4s and 1m03s, then ran the 250ms test four times.
`brokkr test` drops each entry's `tests = ["native_stack"]` selection
(`decide_sweeps` in brokkr's `src/test_cmd/plan.rs` resets `s.filters`) and
honours the entry's `profile = "release"` over `[test] debug = true`. So
every focused run on a cold release cache pays about two minutes of fat-LTO,
`codegen-units = 1` builds for a test that has nothing to do with the native
stack. AGENTS.md confirms the focused runner runs all four configured sweeps.
A side effect: a focused run of a dev-only property (the save golden, see
bugs.md "Save bytes differ between dev and release builds") also runs in a
release sweep.

Fix, brokkr side: have the focused runner skip sweeps whose `tests = [...]`
list cannot contain the name.

Fix, dellingr side: put the release sweeps in a profile `brokkr test` does not
walk. Set `[test] default_profile` to a profile referencing only `default` and
`snapshot`, and either a `gate_profile` (lanes including the release entries)
for `brokkr check --gate` or a `release-native` profile run explicitly. Whether
plain `brokkr check` should still include the release sweeps is a policy call.
The check-machinery hunter notes the release native-stack check needs fat LTO
and `codegen-units = 1` only because `docs/limits.md` states the envelope for
"this repository's release build settings".

Enforceable: yes, in config.

## BRK-002 - Workload pins are verified only when someone benchmarks

Reported by: check-machinery, corpus-harness.

brokkr verifies `xxh128`/`hotpath_xxh128` at `brokkr dellingr` preflight
(`brokkr man dellingr`). Nothing in `brokkr check` does. An edit to
`bench/*.lua` or to a pinned `examples/` file that forgets to re-pin passes the
gate, and the next person to bench gets the refusal. The re-registration is
meant to be "the deliberate signal"; today it is a deferred one.

A second effect: the pin covers comments. `examples/globals/write.lua`'s header
is stale (see verif-corpus.md), and correcting it re-pins the workload and
cuts its history, which is exactly why such comments stay wrong.

Fix: verify the pins in the gate. Dellingr can do it alone with a pure-Rust
`xxhash-rust` dev-dependency in `tests/bench_scripts.rs`. brokkr-side, consider
pinning a semantic fingerprint instead (the `analyze_cost` tree
`bench_scripts.rs` already computes, or the bytecode listing) so comment edits
do not cut history; that is a brokkr design call.

Enforceable: the pin check yes. The pin's definition is brokkr's.

## BRK-003 - No brokkr command hands scripts the binary the build made

Reported by: check-machinery.

`scripts/lib.sh::cargo_artifact` (used by `diff_test.sh` without `--bin`,
`diff_gate.sh`, `bench.sh` and `hotbench.sh`) runs raw `cargo build`, which is
foreign to brokkr's host model: brokkr's stray reaper kills cargo processes
without a brokkr owner at any locked command, and its rustc guard refuses
compiles while a hold exists. So a `scripts/bench.sh` run started next to any
`brokkr check` dies, or blocks it. `research/bench_luars.sh` does not build at
all: it runs whatever `target/release/dellingr` holds, which (because the
release sweeps share that uplifted path) is the binary last uplifted by any
release build, possibly from another commit or the `snapshot` feature build
from `release-native-stack-snapshot`.

Fix: scripts take the binary as a required argument; the standalone build path
belongs to brokkr. No `brokkr` subcommand prints an artifact path today, so
that is a brokkr feature request.

Enforceable: a textlint rule banning `cargo ` invocations in `scripts/**` and
`research/*.sh`.

## BRK-004 - brokkr passes workloads by absolute path, so chunk names and stored targets carry the checkout path

Reported by: check-machinery, corpus-harness.

AGENTS.md says the harness compiles the script "with its path, as given, as the
chunk name, so error locations read `bench/arithmetic.lua:12:`". The brokkr
manual says brokkr passes `/abs/path/to/workload.lua`, and the measure log
confirms it (`target=/home/folk/Programs/dellingr/bench/method.lua`). Under the
documented caller, error locations and the stored `target=` KV carry the
developer's absolute checkout path, and the `target` column in `results.db`
differs between checkouts and hosts for the same workload.
`hotpath_keeps_the_callers_script_path` (which never runs; see the hotpath unit
tests entry in verif-corpus.md) tests only the relative case. The
corpus-harness hunter adds, unverified: if the chunk name is interned in the GC
heap, `setup_heap_bytes`/`final_heap_bytes` (stored sidecar counters) also vary
with checkout path length.

Fix: the harness takes the workload name or strips the project root and uses a
root-relative chunk name; or brokkr passes the registered relative path. Either
way the AGENTS.md claim and the behaviour should agree.

Enforceable: a harness unit test (once the harness tests run) that an absolute
path under the manifest dir yields a root-relative chunk name.

## BRK-005 - Results rows may not record the host's rustflags

Reported by: corpus-harness.

`~/.cargo/config.toml` on plantasjen sets `-Ctarget-cpu=native`, links with
`wild` via `clang`, and sets `[profile.dev] debug = "line-tables-only"`. Hotpath
and bench walls are taken with native CPU tuning. That is a valid choice, but
it is a host property the results rows do not record unless brokkr's env
fingerprint includes rustflags; the hunter did not confirm which. (The effect
of the same flags on the native-stack envelope is in verif-native-stack.md.)

Fix: record effective rustflags in the env fingerprint, if it does not already.

Enforceable: by brokkr.

## BRK-006 - Sweeps cannot select the lib unit-test target or exclude tests

Reported by: runtime, library, compiler-cost, snapshot.

Several proposed dellingr checks need sweep-level test selection brokkr may not
offer:

- A `fault-injection` shape restricted to the lib unit tests plus
  `error_boundary` (runtime): whether `[[check]] tests = [...]` can name the
  lib target could not be told from `brokkr.toml`.
- Running the crmath golden in a release shape (library): the golden is a lib
  unit test, so this needs brokkr support for selecting the lib target in a
  shape, or the crmath crate split in rewrites.md.
- Skipping the compiler tests in the snapshot shape (compiler-cost), which run
  identically in both: "if brokkr grows" a `tests = [...]`-style exclusion.
- Restricting the snapshot sweep to what can differ (snapshot), as the release
  sweeps already do with `tests = [...]`.

Fix: confirm what brokkr supports; add lib-target selection and test exclusion
if absent.

Enforceable: by brokkr config once supported.
