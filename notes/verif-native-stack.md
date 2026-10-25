# Verification: native stack and limits

Findings from the verification hunt about the native-stack envelope and the
limit tests: `tests/native_stack.rs`, `reference/native-calls.md` and
`reference/native-calls.toml`, `tests/common/stack.rs`, the at-cap and
iterative-walker tests, and the claims in `docs/limits.md`. Each entry names the
scopes whose hunters reported it.

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

## VN-007 - The envelope depends on build inputs the repository neither pins nor records

Reported by: native-stack-errors, corpus-harness.

The first two findings (no toolchain pin; host rustflags outside the
repository) were decided: no pin and no override, and `docs/limits.md` now says
the envelope is verified on the developer's machines with their toolchains and
global cargo configuration. What remains is the third:

- `reference/native-calls.md` says "All 57 scenarios were rerun"; the inventory
  has 55 scenarios (57 matches the probe's rows, which include two `chain_`
  rows). The file has become a measurement log (four dated sections, several
  superseded, rows for functions that no longer exist) inside a `reference/`
  document that must be true, and a reader cannot tell which table is current.

Fix: move the measurement history to `reference/performance.md` (the folder
table's home for measured numbers over time), leaving native-calls.md with the
guard and one current table; drop the scenario count; record `rustc -V` with
measurements.

Enforceable: editorial.

Decision (2026-10-25): split by durability. `reference/native-calls.md` keeps
the guard documentation plus one current table of per-level native bytes,
stated with commit, host and `rustc -V`. The dated measurement history moves to
`reference/performance.md` as a dated log; superseded rows and rows for
functions that no longer exist are dropped, not moved. The scenario count goes.
`docs/limits.md`'s envelope section points at the current table instead of
restating figures.

