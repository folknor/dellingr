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

## MATH-006 - FMA dispatch cost on generic x86_64 builds

Raised as a performance observation outside the defect bar.

`fma_dispatch` calls `x86_fma`, a `#[target_feature(enable = "fma")]` function,
from a caller without that feature. LLVM cannot inline across that boundary, so
unless the build sets `-C target-feature=+fma` or a `target-cpu`, every
`crmath::fma` is an out-of-line call plus a cached-CPUID atomic load and branch.
The kernels make tens of FMA calls per evaluation. Multiversioning whole kernels
(one `#[target_feature]` body plus one soft body, selected once per kernel call)
would keep `vfmadd` inline without changing a bit.
