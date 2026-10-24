# Deterministic math

Every transcendental result a script can observe comes from `src/crmath/`:
`math.sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `exp`, `log`
(natural and base 10) and the `^` operator (`OP_POW`). Nothing in the library
calls the platform libm for these, and `clippy.toml` bans the `f64`/`f32`
transcendental methods, `powi` and `mul_add` so new code cannot slip back.

The lints see only this crate's source, not dependencies or the libcalls LLVM
emits, so `scripts/libm_symbols.py` checks the linked binary for platform libm
transcendental symbols (`fmod`, `sqrt` and the other exact operations are
allowed). `brokkr check` runs it, through the integration test
`corpus::libm_symbols`, on the dev-profile CLI binary of the default and
`snapshot` sweeps. The dedicated `libm_symbols` test target also checks the
release binaries in `release-native-stack` and
`release-native-stack-snapshot`, using each sweep's `CARGO_BIN_EXE_dellingr`.
Only code retained in the linked CLI is covered; discarded library code is
outside this symbol check.

## Why

Game replays must reproduce bit for bit on any host. The Rust `f64` methods
lower to the host libm, or to compiler-builtins' weak symbols depending on
link order, and different libms round differently in the last place, so a
replay recorded on one host could branch differently on another.

## Two properties

1. **Same bits on every supported host.** Each kernel is a fixed sequence of
   operations whose results IEEE 754 specifies exactly: binary64 `+ - * /` and
   `sqrt`, integer and bit manipulation, and fused multiply-add through the
   kernel's FMA backend. Rust never contracts `a * b + c` into an FMA and never
   reassociates floating-point expressions, so the sequence written is the
   sequence executed. This property does not rely on the kernels being
   correctly rounded.
2. **Correct rounding.** The kernels are ports of CORE-MATH's binary64
   implementations, which certify the rounding of every result. Correct
   rounding has one valid answer, so it also matches any other correctly
   rounded libm, and replacing the implementation with another correct one
   would not change replays. Two qualifications:
   - `pow` and `atan2` end in a 256-bit or 192-bit phase whose final rounding
     test could, in principle, fail; upstream then aborts. No binary64 input
     is known to get there (CORE-MATH's worst-case files pass). The port
     rounds the high-precision approximation instead, which keeps property 1
     but could be off by one unit in the last place. A later correction of
     such a case would be replay-visible.
   - `math.log(x, base)` for a base other than 10 is `ln(x) / ln(base)`, each
     logarithm correctly rounded and the quotient rounded once more.

## FMA

The FMA every kernel uses must be the correctly rounded IEEE operation
everywhere, since CORE-MATH's error analysis depends on it:

- x86/x86_64: the `vfmadd` instruction, chosen at compile time when the build
  targets FMA and otherwise after run-time CPU detection (std caches it);
- aarch64: `f64::mul_add`, which always lowers to `fmadd` there;
- elsewhere, and on x86 CPUs without FMA: `crmath::soft_fma`, an integer
  implementation.

All of these return the same bits, so the choice never reaches results.
`f64::mul_add` is not used where it could become a library call: the `fma`
symbol it would reach may be compiler-builtins' generic one, which is not
correctly rounded (rust-lang/compiler-builtins issue 1353).

Each elementary kernel selects its backend once at entry, not per operation.
A macro instantiates one source body per backend, shared floating-point
helpers included: a software instance on `soft_fma` everywhere, and a
hardware instance on x86 and aarch64. The entry point takes the hardware
instance when the check above allows it. On x86 every hardware helper carries
the FMA target feature, so the instruction inlines throughout the kernel
without a CPU check per operation. `crmath::fma`, the per-call dispatcher,
exists only in test builds.

## Supported hosts

Binary64 arithmetic exactly as IEEE 754 specifies, with gradual underflow and
the default floating-point environment (round to nearest, no flush-to-zero or
denormals-are-zero, exceptions masked). Rust assumes the same environment.
On x86_64 and aarch64 dellingr reads the thread's control register at its
entry points and refuses anything else (`src/fp_env.rs`;
`docs/determinism.md`, "Floating-point environment"). x87-only targets (32-bit
x86 without SSE2) evaluate with excess range and are not supported; `src/lib.rs`
refuses to compile for them. NaN sign and payload bits
are not part of the guarantee: they never affect a non-NaN result, every NaN is
treated as quiet (so `pow(NaN, 0)` and `pow(1, NaN)` are 1 whatever the
NaN's bits), and the VM never lets a script observe them.

## Porting conventions

- The port is literal: C unsigned arithmetic becomes wrapping arithmetic,
  shifts that C leaves undefined at the limb width are made total, and every
  `__builtin_fma` becomes a call to `fma`, which each kernel instance binds
  to its backend.
- Hex float literals are written as `h!("0x1.921fb54442d18p+0")`, evaluated at
  compile time by `crmath::hf`, which rejects any literal that is not exactly
  representable.
- The constant tables in `src/crmath/tables/` are generated from the upstream
  C sources by `scripts/crmath_tables.py` and are not edited by hand. Run
  `python3 scripts/crmath_tables.py --check` to verify the pinned upstream
  commit and source files, the table registry, and every generated table byte
  for byte; `--generate` rewrites the tables from that same pinned checkout.
  `reference/core-math-pin.toml` single-sources the commit, clone URL and
  reviewed table SHA-256 digests. The ordinary, checkout-free
  `core_math_table_digests_match_pin` test runs
  `python3 scripts/crmath_tables.py --check-pins` (Python 3.11 or later).
  An intentional table update must review and update those digests alongside
  regeneration; the generator does not rewrite the manifest.
- Directed rounding, `errno`, exception flags and upstream's exception-flag
  save/restore are removed; only round-to-nearest is implemented.
- The upstream commit and clone URL are in `reference/core-math-pin.toml`;
  licence notices are in `src/crmath/LICENSE-CORE-MATH`.

## Tests

- `crmath::tests::fma`: `soft_fma` against the hardware instruction and
  against exact big-float arithmetic, including the tie and overflow shapes
  that broke compiler-builtins.
- `crmath::tests::golden`: `tests/fixtures/crmath_golden.txt` pins special
  values, random inputs and a sample of CORE-MATH's worst cases for every
  kernel. Each line is checked under every FMA path the host can run. The
  same file has to pass on every supported host; it is the cross-platform
  guarantee in executable form. These forced paths go through test-only
  dispatch hooks, so the integration test `library::crmath_golden` also runs
  every line through the shipped build: `math` functions and `^` from a
  script. Both run in the dev sweeps and, optimized, in the `release-goldens`
  check shape.
- `crmath::tests::oracle`: an interval Ziv procedure over `libbeef` in
  directed rounding, which certifies the expected values. It reports an error
  instead of guessing if its explicit Ziv loop exceeds its precision cap: 512
  bits for pi-dependent functions and 4096 bits for the others. This cap does
  not constrain precision escalation internal to a `libbeef` primitive.
  The `regenerate_crmath_golden` example certifies the golden inputs and
  writes changed bytes to an explicit output path:
  `brokkr run --release regenerate_crmath_golden -- tests/fixtures/crmath_golden.txt`.
  Long oracle sweeps and checkout-dependent checks in `crmath::tests::golden`
  require the explicit `crmath-sweeps` feature. Run them in release with
  `brokkr test crmath::tests::golden --release --features crmath-sweeps`.
  The worst-case sweeps read the checkout under `research/core-math`.
  The checkout must be the upstream commit in `reference/core-math-pin.toml`,
  the one the port was taken at (which the golden file's header also names).
  The reader pins each function's parsed input
  count and a digest of those inputs, so the sweeps, the regeneration and
  the cheaper `core_math_corpus_matches_pin` fail on a missing,
  damaged or different corpus instead of testing other inputs; nothing
  else about the checkout is verified. The worst-case files are read to the
  values CORE-MATH's own checkers get from `%la`: a trailing annotation after
  the number is ignored and a hex literal is rounded to nearest binary64, so
  pow's inexact subnormal bases test their rounded value. The reader covers
  the shapes those files use, not all of `%la`; a line it cannot read fails
  the sweep rather than being skipped.
