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

## VG-009 - Source-only checks compile and run as integration tests in every sweep

Reported by: check-machinery.

These inspect files only, yet are compiled and run once per dev sweep: every
test in `corpus::test_conventions` (more of them since the integration merge).
They fail late, after clippy and a full build. (The DIFF/SKIP-DIFF marker
reason rule already has its pre-clippy home, the `diff-marker-needs-reason`
textlint rule.)

Fix: textlint rules, or one pre-clippy `script_check`, for the conventions.

Enforceable: yes.
