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

# Deterministic math defects: crmath kernels, oracle, table generation

Filed from the 2026-10-23 defect hunt at a22d9c0 (scope `crmath`). The hunter
read the code statically and built or ran nothing. The hunter found `det_hash`
consistent: the aliases fix the hasher, only `RustFn` keys are address-hashed
(as the module doc says), and no hash value reaches order or snapshots.

## MATH-005 - `math.log` comment overstates "as in reference"

- **`math.log` comment** (lua_std/math.rs, stdlib scope; noted because it
  describes crmath use). It says "Reference special-cases base 10 with log10 ...
  only the literal base 10 takes that path, as in reference". That is Lua 5.2;
  Lua 5.4 also special-cases base 2 with `log2`. docs/compatibility.md states the
  divergence correctly; only the code comment overstates "as in reference". The
  script-visible effect of the documented divergence: `math.log(2^k, 2)` can be
  `k +- 1 ulp` (Python's equivalent famously gives `29.000000000000004` for
  `k = 29`), so `floor(math.log(n, 2))` bit counting can be off by one. Because
  that divergence is documented, the hunter does not count it as a defect.

## MATH-006 - FMA dispatch cost on generic x86_64 builds

Raised as a performance observation outside the defect bar.

`fma_dispatch` calls `x86_fma`, a `#[target_feature(enable = "fma")]` function,
from a caller without that feature. LLVM cannot inline across that boundary, so
unless the build sets `-C target-feature=+fma` or a `target-cpu`, every
`crmath::fma` is an out-of-line call plus a cached-CPUID atomic load and branch.
The kernels make tens of FMA calls per evaluation. Multiversioning whole kernels
(one `#[target_feature]` body plus one soft body, selected once per kernel call)
would keep `vfmadd` inline without changing a bit.

## MATH-007 - Upstream `is_exact` quirk confirmed to have no observable effect

Raised outside the defect bar. notes/todo.md already tracks the upstream
`is_exact` quirk (it measures the bit length of `m`, not of `m^y`). The hunter
confirmed that it only feeds `exact`, which gates the redundant `!exact` check in
`exact_pow`, and found no observable effect.

## MATH-008 - The crmath golden regenerator drops hand-added regression rows

Lateral from the first resolution wave. `regenerate_crmath_golden`
(src/crmath/tests/golden.rs) rebuilds `tests/fixtures/crmath_golden.txt` from
its own input generator, so a row added by hand is lost on the next
regeneration. The overflow witness `pow(0x1.62a88613629b6p+678, 0x1p+1014)`
(expected `+inf`) is such a row today. Regeneration should carry an explicit
list of pinned regression inputs, that witness included.

## MATH-009 - The oracle's precision cap bounds only its own Ziv loop

Lateral from the first resolution wave. `certify` in
src/crmath/tests/oracle.rs now stops pi-dependent functions at 512 bits and
returns `Err` above it. libbeef's own internal Ziv procedure can still raise
precision on its own past that, where its `pi` breaks (from 688 bits). Latent,
since no binary64 input needs that much; worth a comment at the cap saying the
dependency's internal escalation is not covered.

## MATH-010 - reference/deterministic-math.md does not describe the table check or the oracle cap

Lateral from the first resolution wave. `scripts/crmath_tables.py` now records
every table spec and has `--generate` and `--check` modes against
`research/core-math` at the pinned commit, backed by the ignored
`core_math_tables_match_pin` test, and the oracle caps pi-dependent functions at
512 bits. The reference page still says only that tables "are generated ... by
`scripts/crmath_tables.py`" and that the oracle certifies; it should say how
generation is checked and where the oracle reports instead of guessing.
