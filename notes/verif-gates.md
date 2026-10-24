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
test in
`corpus::test_conventions` (more of them since the integration merge),
`diff_test.rs::diff_markers_have_nonempty_reasons`,
`diff_test.rs::shell_and_rust_example_corpora_match`, and
`diff_test.rs::malformed_timeout_is_rejected...`. They fail late, after clippy
and a full build.

Fix: textlint rules, or one pre-clippy `script_check`, for the marker-reason
rule and the conventions. The corpus-equivalence test exists only because two
walkers exist; the in-process gate rewrite in rewrites.md removes it.

Enforceable: yes.

## VG-020 - The rules forbid the benchmarking tools the docs prescribe

Reported by: check-machinery.

The CLAUDE.md benchmarking rules say "Never hyperfine, `scripts/bench.sh`,
`scripts/hotbench.sh`". AGENTS.md documents both scripts as the tools, and
`brokkr man dellingr` says comparison against reference Lua is "not part of
this surface - `scripts/bench.sh` in the dellingr repo owns that". The README
ratio table and the `lua-rs` column (via `research/bench_luars.sh`) can
therefore only be produced by a tool the rules forbid.

`hotbench.sh` is fully superseded by `brokkr dellingr --lua W --hotpath`; its
hyperfine half times the ms-scale `hotpath_file`, which the same rules say is
below launch noise. `scripts/diff_gate.sh` is invoked by nothing, and `brokkr
test differential_examples --debug` covers its job.

Fix: delete `hotbench.sh` and `diff_gate.sh`. Decide whether vs-Lua ratios are a
measurement the project still publishes; if so the rule needs an explicit
carve-out, and the tool needs the same provenance discipline (hostname,
commit) as `brokkr results`.

Enforceable: partly. A deleted script cannot drift.
