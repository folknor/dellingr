# Verification: corpus and harness

Findings from the verification hunt about everything that runs the
`examples/` and `bench/` corpora: the integration crate's `corpus` subject
(`run_examples.rs`, `bench_scripts.rs`, `disassembly.rs` under
`tests/integration/corpus/`), `scripts/diff_test.sh`, `examples/hotpath.rs`,
the `cli` subject, `tests/integration/support/` and `tests/common/`, and the
Lua files themselves. Some entries would be removed by the expected-output
rewrite in rewrites.md; they stay filed here until then. Each entry names the
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

## VC-003 - What `run_examples` accepts as a pass, and the DIFF/SKIP-DIFF files nothing else checks

Reported by: check-machinery, corpus-harness, library.

An example passes when it exits 0, writes no stderr and prints no line ending
in `: false`; a `_bench` file must also print a line ending in `: true`. Under
that rule a condition that evaluates to `nil` prints `name: nil` and passes,
and a non-bench example that prints nothing passes.

A `-- DIFF:` or `-- SKIP-DIFF:` marker removes the reference comparison for the
whole file. The corpus test then checks of a DIFF file only that its output
matches neither reference, which any single divergent line satisfies, so for
these files the `: false` rule is all that remains. One divergence removes
every other assertion in the file from differential coverage. Several marked files print in shapes that rule never sees:

- `examples/math.lua` (DIFF, number formatting and RNG): prints `label =\t<value>`
  via `print(a, b)`. No line can end in `: false`; every `math.sin(...)` and
  `math.random()` value is printed and compared against nothing.
- `examples/edge_cases.lua` (DIFF, because 0/0 renders `nan` where C Lua prints
  `-nan` on x86-64): about 70 checks as
  `print("10 < 9 = " .. tostring(10 < 9))  -- false`, expected values only in
  comments, `= false` is not `: false`, and the closing banner is
  unconditional. One divergent line disables comparison of the other ~70.
- `examples/upvalue_stress.lua` (SKIP-DIFF): ~30 value prints with expected
  values in comments, banner unconditional; nothing in it looks
  dellingr-specific.
- `examples/stress_tables.lua`, `stress_rustfn.lua`, `stress_gc_stack.lua`
  (SKIP-DIFF): progress banners and an unconditional `PASS`. Smoke tests by
  design, but nothing records that.
- `feature_test.lua` is excluded for a global `unpack`,
  `feature_test_extended.lua` for semicolons.

Fixes, in the hunters' order of preference: split the divergent statement into
its own small DIFF file and drop the marker from the rest (`edge_cases.lua`:
move the `0/0` line out; `math.lua`: move the random and float-rendering lines
out); drop markers that are not needed (VC-004); convert value prints to the
`name: true/false` convention or `error()` on mismatch as `feature_test.lua`
does; or give DIFF files a checked-in `.expected` fixture compared byte for
byte (the expected-output rewrite in rewrites.md). Short of that,
`run_examples` could require every non-blank stdout line of a `name: value`
example to end in `: true`.

Enforceable: yes. In `run_examples.rs`, require every DIFF/SKIP-DIFF file to
print at least one line matching `: (true|false)$` or call `error(`, unless it
is on an explicit smoke-test allowlist; or require every DIFF file to have an
`.expected` sibling.

## VC-019 - Tests that spawn tools rather than the product

Reported by: check-machinery, corpus-harness.

The `test-processes-through-support` textlint rules in `brokkr.toml` keep
every process a Rust test starts in `tests/integration/support/process.rs`
(the CLI and the reference Luas), except two `python3` spawns that carry a
`process-spawn-exception:` marker: `tests/common/libm_symbols.rs` runs
`scripts/libm_symbols.py` (which runs `nm`) on a linked CLI binary, and
`src/crmath/tests/golden.rs` runs `scripts/crmath_tables.py --check-pins` (and
`--check` under `crmath-sweeps`). Undecided: keep them as test-time spawns,
port them to Rust, or move them to script checks (the libm one needs the
binary of the sweep that built it, which is why it is a test). When decided,
drop the marker and the rule's exception with them.

Decision (2026-10-25): no Rust code spawns anything but dellingr and the
reference Luas.
- Port the libm symbol check to Rust: read the ELF symbol table with the
  `object` crate as a dev-dependency, port `scripts/test_libm_symbols.py`'s
  fixtures, and drop `nm` from `scripts/check_tools.py`.
- `crmath_tables.py --check-pins` becomes a pre-clippy `[[script_check]]`.
- `--check` becomes a documented manual command run when regenerating
  tables, not a feature-gated test.
- Delete the `process-spawn-exception:` marker and the rule's two
  allowances.
