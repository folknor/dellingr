# Defect hunt: crmath (deterministic math) and det_hash

Scope: `src/crmath/*` (kernels, dint/qint/trig_dint, generated tables,
tests and oracle), `scripts/crmath_tables.py`, `src/det_hash.rs`, and the
point where `src/lua_std/math.rs` and `OP_POW` hand values to crmath.
Contract: `reference/deterministic-math.md`, `docs/determinism.md`,
`docs/compatibility.md`, the `crmath` module doc.

Everything here comes from reading the code. Nothing was built or run. The
witness in finding 1 is derived by hand from upstream's documented
worst case. A one-line test would confirm it.

---

## 1. `pow`: the overflow sentinel from `exp_2` goes into the phase-2 rounding test, `exact_pow` and phase 3 (wrong results, and a debug-build panic reachable from Lua)

**Claims broken**

- `reference/deterministic-math.md` "Correct rounding": the only stated way
  `pow` can fail to round correctly is a failed final 256-bit rounding test,
  and then it is "off by one unit in the last place".
- `docs/determinism.md`: `^` "possibly one unit in the last place off".
- The `pow` doc comment: "Correctly rounded x^y (see the module doc for the
  residual case)".
- The project-wide model that script errors are `Err`s that kill the
  callback, not host panics.

**Mechanism** (`src/crmath/pow.rs`)

1. Phase 1 sets `lh = ll = NaN` when `y`'s biased exponent is outside
   `0x36..0x7f5`, so for every `|y| >= 2^1014` (and every `|y| < 2^-969`)
   phase 1 always falls through to phase 2.
2. For `|y| >= 2^1014` and `x != 1`, `|y * ln x| >= 2^961`. `exp_2` takes its
   "Underflow or overflow" shortcut (`x.ex >= 10`). It returns `*x` with only
   `ex` overwritten (`1025` for overflow) and `sgn` cleared. So the
   significand `hi:lo` is the significand of `y * ln|x|`, not of the result.
3. `pow` catches only the underflow form (`r.ex < -1075`). The overflow form
   goes straight into the phase-2 rounding test
   `lo = (r.lo >> 10) | (r.hi << 54); rd = lo + 28 > 56`. That test reads the
   64 bits after the round bit of the *logarithm product*. Whenever those
   bits are within +-28 of zero, `rd` is false and the code continues as if
   x^y were near a rounding boundary.
4. From there:
   - **`x` a power of two** (`m == 1` in `exact_pow`): `g = E * y` is a
     huge integer-valued float. `g as i64` saturates to `i64::MAX`, and
     `pow2(±1, i64::MAX)` builds its scale from
     `((e >> 1) + 0x3ff) & 0x7ff = 0x3fe`. That returns **±0.5** where the
     answer is ±inf. (Upstream C has UB in the same cast. x86 gives
     `INT64_MIN`, so upstream returns 1.0.)
   - **Any other `x`**: `exact_pow` returns `None` (`y > 34`). Phase 3 calls
     `exp_3` on `|qR| ~ 2^1022`, far outside its documented domain
     (`|x| < 744.45`, no overflow guard). `qint_toi` then evaluates
     `a.hh >> (63 - a.ex)` with `a.ex ~ 1034`. That is a negative shift
     amount: **a panic ("attempt to shift right with overflow") in any build
     with overflow checks (dev/test)**. In release the shift masks to 53 and
     the rest of `exp_3` runs on garbage. As far as I can follow it, the
     result then ends up +inf only because the garbage exponent is huge and
     `qint_tod` saturates. That is luck, not design.

**Concrete witness (non-power-of-two branch).** CORE-MATH's `log.c`
documents `x = 0x1.62a88613629b6p+678` as the hardest log input: "64
identical bits after the round bit". Those are exactly the 64 bits that
`lo` extracts (bits 54..117 of the 128-bit significand). `pow`'s `log_2` is
accurate to 2^-122.88, so it keeps them (a carry or borrow from below only
moves `lo` by 1). Multiplying by `y = 2^1014` (`dint_fromd` gives
`hi = 2^63`) changes only the lowest bit. `y`'s biased exponent is exactly
`0x7f5`, so phase 1 defers. So:

```rust
crmath::pow(hf("0x1.62a88613629b6p+678"), hf("0x1p+1014"))  // expected +inf
```

should panic in the dev profile. From Lua:
`(0x162a88613629b6 * 2^626) ^ 2^1014` (`2^626` is exact). The oracle
certifies `+inf` for this input (its `exp_bound` maps `z > 1e4` to infinity),
so adding it to `tests/fixtures/crmath_golden.txt` would catch it. The golden
test runs in the dev profile, which is the only profile where it panics.

**How often.** Each `(x, y)` with `|y| >= 2^1014` whose product overflows
lands in the `rd`-false window with probability about 57/2^64. But the
input space is about 2^62 x values times about 2^55 y values, so there are
astronomically many such inputs, and a hard-to-round log gives them on
demand (above). For the power-of-two branch I have no witness. It needs
`e * n * ln2` (with `n` the odd significand of `y`) to have 58+ identical
bits after its 54th bit. With about 2^63 candidate `(e, n)` pairs, expected
count is about 30. A continued-fraction search per `e` would find them.

**Why the tests miss it.** The worst-case sweeps and `random_against_oracle`
run in release, where the panic becomes a masked shift. The pow worst-case
corpus has about 50 lines with `|y|` near 2^1015 to 2^1023, and none sit in
the `rd`-false window. The random inputs hit the window with probability
about 2^-58.

**Fix direction (structural).** Stop encoding "overflow" as a `Dint` with a
meaningless significand. When phase 1 defers because `y` is out of range,
the answer is already known: for finite nonzero `x != 1`, the sign of
`y * ln|x|` decides overflow (±inf with sign `s`) or underflow (±0), and
`|y| < 2^-969` gives exactly 1.0. Decide it there, or have
`exp_2`/`exp_3` return an `enum { Finite(Dint), Overflow, Underflow }`. Then
nothing downstream (`rd` test, `exact_pow`, phase 3) can see an
out-of-domain value. While there:

- `exact_pow`'s `m == 1` branch should reject `|g| > ~1100` before
  `g as i64`.
- `dint_tod`/`qint_tod`'s `ex > 1024` branch assigns `r = f64::MAX` and
  loses the sign. This is unreachable today because huge `y` is even so
  `s = 1`, and it is copied from upstream, but it is the same fragile
  sentinel handling.

---

## 2. `pow` phase 3: the near-boundary window is mis-specified (inherited from upstream; no observable effect found)

`near_boundary` uses `!r1 == 0 && !r2 == 0 && r3 + 120 <= 60`, which accepts
`r3` in `[2^64-120, 2^64-60]`. The tail it is meant to catch, within 60
units below a boundary, is `r3` in `[2^64-60, 2^64-1]`. Phase 2's analogous
`lo + 28 > 56` has the right shape. Upstream `pow.c` has the same line, so
the port is literal.

Effect in the port: a tail within 60 units below a boundary is rounded
directly. A tail 60..120 below is flagged unnecessarily. The flagged path
then either returns 1.0 or reaches the same `qint_tod` (the low-10-bit mask
does not matter when `r1`/`r2` are all ones). So I could not construct an
observable difference. It is still a test that does not test what its
comment (and the module doc's "final rounding test could fail") says.

Related comment inaccuracy: the `q_r.ex < -56` branch comment says it
"rounds to the neighbour of 1 on qR's side, or to 1". In the
round-to-nearest-only port, `1.0 + 0x1p-100` and `1.0 - 0x1p-100` both
evaluate to exactly `1.0`. The branch always returns 1.0, which is correct
for `|qR| < 2^-56`. Upstream's `±2^-100` exists for directed rounding and
flags, which the port removed.

---

## 3. Test oracle: past 688 bits it can "certify" a wrong value instead of reporting

**Claim broken:** `oracle.rs` module doc: "Otherwise double p, up to a cap;
reaching the cap is reported, never guessed". Also
`reference/deterministic-math.md`: the oracle "certifies the expected
values".

`notes/todo.md` records that libbeef's `pi` returns 0 from 688 bits up,
"taking atan with it". `trig_reduced` works around that for the outer
reduction only. The Ziv loop in `certify` still doubles `p` through 1024,
2048 and 4096 for every function, and then calls libbeef's
`atan`/`asin`/`acos`/`atan2`/`sin`/`cos`/`tan` directly, which reduce by
`pi` internally. If an input ever needs `p > 512`, both bounds come from the
same broken primitive. They can round to the same wrong binary64, and
`certify` returns `Ok(wrong)`.

In practice no binary64 input needs more than about 256 bits (worst cases
sit around 2^-126 relative), so this is latent. But the guarantee "never
guessed" does not hold past 688 bits. Capping `p` at 512 for the
pi-dependent functions, and returning `Err` above it, would make the doc
true.

---

## 4. `scripts/crmath_tables.py` does not match its own docstring, and table generation cannot be reproduced

**Claim broken:**

- The script docstring: "The Rust source is printed on stdout" and "The
  source tree is read-only input; nothing here writes files". `main()` also
  accepts `--out=<path>` (it writes the file) and `--use=<path>`. Neither
  is documented in the usage line.
- `reference/deterministic-math.md`: the tables "are generated from the
  upstream C sources by `scripts/crmath_tables.py`".

Nothing in the repo records the spec lists (C file, table name, Rust name,
kind, struct) that produced each `src/crmath/tables/*.rs`. Examples:
`trig.rs` mixes `cos/cos.c` and `tan/tan.c`, `asin.rs` mixes `asin.c` and
`acos.c`, and `pow.rs` mixes `pow.h` and `dint.h`. There is no `--check`
gate either. Compare `examples/parse/generate.py --check`, which
`brokkr check` runs. So "generated, not hand-edited" is an unverifiable
assertion today: a hand edit to a table would pass every gate except the
release-only sweeps.

Structural fix: put the per-file spec lists in the script itself, make it
regenerate every table file, and add a `--check` mode against
`research/core-math` at `CORE_MATH_COMMIT`. The check needs the checkout,
so it can be an ignored test next to `core_math_corpus_matches_pin`.

---

## 5. Smaller doc and comment inaccuracies

- `src/crmath/mod.rs` and `reference/deterministic-math.md` say no kernel
  branches on NaN sign bits. `ln`/`log10` do branch on it (`e == 0x400`
  versus `e == 0xc00 && x != -inf`). Both arms return NaN, so the property
  that matters (NaN bits never reach a non-NaN result) holds. The sentence
  as written does not.
- `lua_std/math.rs` (stdlib scope, noted because it describes crmath use):
  the `math.log` comment says "Reference special-cases base 10 with log10
  ... only the literal base 10 takes that path, as in reference". That is
  Lua 5.2. Lua 5.4 also special-cases base 2 with `log2`.
  `docs/compatibility.md` states the divergence correctly. Only the code
  comment overstates "as in reference". Script-visible effect of the
  divergence: `math.log(2^k, 2)` can be `k ± 1 ulp` (Python's equivalent
  famously gives `29.000000000000004` for `k = 29`), so `floor(math.log(n, 2))`
  bit counting can be off by one. That divergence is documented, so it is
  not counted as a defect here.

---

## Lateral observations (not defects against a stated contract)

- **FMA dispatch cost on generic x86_64 builds.** `fma_dispatch` calls
  `x86_fma`, a `#[target_feature(enable = "fma")]` function, from a caller
  without that feature. LLVM cannot inline across that boundary, so unless
  the build sets `-C target-feature=+fma`/`target-cpu`, every
  `crmath::fma` is an out-of-line call plus a cached-CPUID atomic load and
  branch. The kernels make tens of FMA calls per evaluation. Multiversioning
  whole kernels (one `#[target_feature]` body plus one soft body, selected
  once per kernel call) would keep `vfmadd` inline without changing a bit.
- **Flat-charged `^` with extreme exponents does disproportionate work.**
  `|y| < 2^-969` always runs all three phases: phase 2 sees a tail of all
  zeros, `exact_pow` declines, and phase 3 reaches the `q_r.ex < -56`
  shortcut. `|y| >= 2^1014` always runs phase 2. The fix in finding 1
  (decide these in phase 1) removes both.
- `math.atan(y, nil)` and `math.log(x, nil)` raise a type error
  (`get_top() >= 2` then `check_type_named`), while Lua 5.4 treats a nil
  second argument as absent. This is stdlib scope, and a script author
  passing an optional base or `x` through would hit it. I did not find it
  in `docs/compatibility.md`.
- `notes/todo.md` already tracks the upstream `is_exact` quirk (it measures
  the bit length of `m`, not `m^y`). I confirmed it only feeds `exact`,
  which gates the redundant `!exact` check in `exact_pow`, and found no
  observable effect.

## Checked and found consistent

- `soft_fma`: special operands, signed zeros (zero addend, exact
  cancellation), sticky alignment, the subnormal grid, overflow to
  infinity. `hf` (exactness, subnormal encoding). `roundeven_finite`.
  `copysign`/`fabs`.
- Table index ranges in `ln`/`log10`/`log_1`/`log_2`, `POW10` indexing,
  `reduce`/`reduce_large`/`reduce_large_acc` limb indices, and the
  `trig_dint::reduce` shift ranges.
- Special values of every kernel against C99 Annex F as encoded in the
  oracle, including `pow`'s documented every-NaN-is-quiet departure.
- `atan2`'s removed abort check (the port rounds exactly what upstream
  rounds when the check passes). `acos_refine`/`atan_refine2` match
  upstream's wrapping arithmetic.
- `det_hash`: `IndexMap`/`IndexSet` aliases fix the hasher. `Str`/`Obj`
  table keys hash slotmap keys, not addresses, so only `RustFn` is
  address-hashed, as the module doc says. The interner's `HashTable` is
  never iterated. No hash value reaches order or snapshots.
- `research/core-math` HEAD matches `CORE_MATH_COMMIT` and
  `LICENSE-CORE-MATH`.
