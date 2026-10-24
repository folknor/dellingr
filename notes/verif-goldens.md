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

Decision (2026-10-24):
- Run the cost golden, the crmath golden and the opcode-encoding fixture in a
  release `[[check]]` with an `only` filter (the save golden already has
  `release-save-golden`), and add a corpus-wide cost golden, every example's
  cost, produced by the corpus gate (`corpus::run_examples`, now one test
  per example).
- Add a `cargo clippy --target aarch64-unknown-linux-gnu` shape so the aarch64
  arms are compiled and linted.
- Add `compile_error!` for `target_arch = "x86"` without `sse2` in
  `crmath/mod.rs`.
- Optional, alongside the aarch64 shape: one `dispatch!` macro replacing the
  per-kernel dispatch copies.
- musl, i686 and qemu runs only if a player platform needs them.

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

Decision (2026-10-24, provisional): probe at `State` creation and every call
into Lua (`black_box`ed checks for FTZ, DAZ and directed rounding), returning a
dedicated host-environment error that is charged nothing and is not an ordinary
script error. It lives in the prologue every public fallible method already has
for the poison contract. A test sets FTZ on x86_64 and expects the refusal.
CHANGELOG entry; `docs/determinism.md` says the contract is enforced. Resetting
MXCSR on entry is rejected: changing the FP environment is undefined behaviour
in Rust, and the reset is x86-only.

## VD-019 - `%` and `math.fmod` lower to the platform `fmod` with no edge-case golden

Reported by: library.

`docs/determinism.md` states this correctly, and IEEE `fmod` is exact, so correct
libms agree. There is still no golden of remainder edge cases (signed zeros,
subnormal divisors, huge quotients) to show it on a given host. The libm symbol
check in verif-gates.md would at least record `fmod` as the one permitted libm
import.

Enforceable: yes, by a golden.
