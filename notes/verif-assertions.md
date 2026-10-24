# Verification: assertions

Findings from the verification hunt about individual tests: tests that cannot
fail, that check something weaker than their name, that rely on a setup step
that may do nothing, or that sit under the wrong subject. Grouped by subsystem
after a cross-cutting section. Each entry names the scopes whose hunters
reported it.

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

# Cross-cutting

## VA-009 - Boolean-collapsed script results

Reported by: runtime.

Many tests return `cond and 1 or 0` or one boolean from a long chain of checks, so
a failure says only `0 != 1`. The `error("what broke")` pattern some tests use
(`global_table.rs`, `table_metamethods.rs`) reports which check failed at no
extra cost.

Enforceable: no.

# Errors and boundaries

## VA-016 - Smaller items in the error suites

Reported by: native-stack-errors.

- `vm_fault_context.rs` defers to the unit test
  `internal_fault_keeps_innermost_instruction` by name; it exists today in
  `src/vm/tests.rs`, but a rename leaves the comment pointing at nothing. Low
  value; not enforceable short of a grep.
- `error_boundary.rs` sets up the `large` global only when the case source
  contains the substring `large`. It fails loudly when wrong; setup keyed on a
  name.

Enforceable: no.

# Runtime

## VA-031 - Growth-limit coverage exists only under `snapshot`

Reported by: runtime.

`sequence_copy_refuses_an_unallocatable_snapshot_gap`,
`snapshot_gap_growth_refuses_append_and_both_window_extensions`,
`snapshot_length_limit_refuses_append_insert_and_migration`,
`host_insert_growth_failure_preserves_operands` and
`snapshot_growth_failure_reaches_the_callback_as_an_ordinary_error` reach
`MAX_ARRAY_LEN` through `Table::rebuild_from_snapshot`, which exists only under
`snapshot`. The growth-failure path is not snapshot-specific (`MAX_ARRAY_LEN` and
`TableError::GrowthFailed` are in the default build), so in the default sweep, the
shipped consumer build, it has no test.

Fix: a `#[cfg(test)]` constructor that sets a table's logical length and window
directly, used by both sweeps; keep one snapshot-specific test for the load path.
(The `Limits` rewrite in rewrites.md is the broader alternative.)

Enforceable: no general rule.

# Compiler and cost

# Library

# Snapshot
