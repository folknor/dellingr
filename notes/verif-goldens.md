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

## VD-007 - Determinism is pinned on one host and one target

Reported by: check-machinery, compiler-cost, library.

Replays must be byte-for-byte reproducible "on every host", and goldens exist
for it. The release profile half is closed: the `release-goldens` and
`release-corpus` check shapes run the cost, crmath, opcode-encoding, save and
corpus cost goldens optimized, and x87-only x86 refuses to compile. What is
still open:

- Target: every sweep runs on one host and one target. There is no CI
  directory. The aarch64 hardware instance (`fma_dispatch` on `mul_add`, under
  `#[allow(clippy::disallowed_methods)]`), each kernel's aarch64 `cfg` arm (11
  copies of about 20 lines of dispatch plumbing, which one `dispatch!` macro
  could replace) and `src/fp_env.rs`'s aarch64 arm are not even compiled or
  linted on an x86 host. A `cargo clippy --target aarch64-unknown-linux-gnu`
  shape (needs the rustup target) would compile and lint them; none of the
  options covers aarch64 execution without a runner. Undecided.
- musl (`x86_64-unknown-linux-musl`, different libc and allocator), i686
  (32-bit `usize`) and qemu-user runs: only if a player platform needs them.
- `examples/hotpath.rs` uses `std::os::unix` and `libc::O_NONBLOCK`, and
  `cargo test` builds examples, so the suite cannot build on Windows.
- Overflow differs between profiles (debug panics, release wraps), and
  `analyze_cost` uses plain `+=` on `u64`; the release cost tests exercise no
  input near the wrap.

Decision (2026-10-25): close the entry with comments, no new checks.
- aarch64: accepted as unverified, in line with VN-007 (verified on the
  developer's machines); say so where the determinism contract lists its
  targets.
- musl, i686 and qemu stay "only if a player platform needs them".
- Windows: no action; note that the library is unaffected and only the
  dev-host test suite is Unix-only.
- `analyze_cost`: a comment at the accumulation stating why it cannot
  overflow, derived from the compiler's actual limits (verify the bound
  before writing it; if it does not hold, report it instead).
## VD-019 - `%` and `math.fmod` lower to the platform `fmod` with no edge-case golden

Reported by: library.

`docs/determinism.md` states this correctly, and IEEE `fmod` is exact, so correct
libms agree. There is still no golden of remainder edge cases (signed zeros,
subnormal divisors, huge quotients) to show it on a given host. The libm symbol
check in verif-gates.md would at least record `fmod` as the one permitted libm
import.

Enforceable: yes, by a golden.

Decision (2026-10-25): a small bit-exact golden of remainder edge cases (signed
zeros, subnormal divisors, huge quotients, infinities, NaN operands) through
both `%` and `math.fmod`, beside the existing replay goldens, so it runs in the
dev and release sweeps. It guards the in-crate floor-sign correction everywhere
and the platform `fmod` on whatever host runs it.
