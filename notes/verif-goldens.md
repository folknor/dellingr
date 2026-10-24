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

## VD-001 - The save golden still pins the whole stream, and its text companion is a hex dump

Reported by: snapshot; narrowed by the resolution loop's reviewer.

`tests/integration/snapshot/save_golden.rs` still compares the whole debug
save, including `CARGO_PKG_VERSION`, `COST_MODEL_VERSION`, the compiler's output
for the golden program and every stdlib registration id, so it must be
regenerated on unrelated compiler, cost, stdlib or release-number changes, and
a traversal reorder made in the same commit is blessed with them. The new
`tests/fixtures/save_golden_current.txt` companion is a hex dump, so a one-byte
insertion shifts every later row and a reorder is still not a readable diff.

Fix: compare the payload after the header; make the text companion a
structural decode (the arena in id order, bytecode as hash plus length) so a
reorder is a readable diff and a codegen change touches one line; optionally
keep a small separate binary golden from a raw State with no closures for exact
codec bytes.

Enforceable: structurally, by the decode-based companion.

## VD-002 - Released-format archive gaps

Reported by: snapshot; narrowed by the resolution loop's reviewer.

`save_released_v6.bin` and `save_released_v7.bin` now pin the shipped formats,
with fingerprints and a load-and-resave check while a listed format matches the
current build. Remaining:

- `tests/fixtures/opcode_encoding.txt` has no per-released-version digest, so
  an opcode renumbering inside a released format is caught only by its
  editable fixture.
- Archives are keyed by format version alone. A release that ships the same
  format with a different cost model has no slot for a second archive, and the
  frozen check skips silently once the cost-model word moves; the release
  procedure does not say what to do.

Fix: pin opcode_encoding.txt per released format the same way; key archives by
(format, cost model) or state the procedure.

Enforceable: by the same test.

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

## VD-021 - The cost fixture fingerprint covers row names and order

Reported by: the resolution loop's reconciler.

`tests/fixtures/cost_golden.txt`'s fingerprint hashes whole row lines,
including each row's name and their order, so renaming or reordering a row
without changing any cost forces a `COST_MODEL_VERSION` bump and a new
`COST_MODEL_HISTORY` pair. Strict and safe, but it ties a cosmetic edit to a
replay-visible version number. Also, nothing checks that a `pin` row's last
column still names a test that reads it, and `pinned_cost` matches by name
across kinds (names are unique today).

Fix: decide whether the fingerprint should cover only (kind, input, expected),
sorted; add a check that every `pin` row is read by some test.

Enforceable: the pin-consumer check by a test; the fingerprint scope is a
design choice.

## VD-022 - The inline-cache differential mostly compares budget-bounded prefixes

Reported by: the resolution loop's reviewer.

`src/vm/cache_differential.rs` runs each corpus example with cache hits on and
off under one small cost budget, so most examples stop at `BudgetExceeded` and
the comparison covers only their first few thousand charged units. Its comment
says it compares "completed programs" as well; how many examples actually
complete is not reported. Differences that only appear after warm-up (the
cases caches exist for) may never be reached.

Fix: report or assert how many examples complete; raise the budget, or run
non-stress examples unbounded, as far as the 20-second cap allows.

Enforceable: by an assertion on the completed count.
