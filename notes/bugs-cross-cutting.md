# Cross-cutting findings: repo rules and the hunt process

Filed from the 2026-11-04 defect hunt. Findings that belong to no single scope.
Hunt reports are work documents and may be wrong; verify before fixing.

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

## X-001 - Lint suppressions in the tree, and two rule sources that disagree about them

Raised by `stdlib-strings` and `snapshot`.

Claim broken: AGENTS.md, "Never disable a lint; fix the code."

- `saturating_usize` in `src/lua_std/string.rs` carries
  `#[allow(clippy::disallowed_methods)]` for `Result::unwrap_or`. The
  `clippy.toml` reason text for that ban says "or use a scoped allow for an
  intentional fallback", so the repo's two rule sources disagree, and one of
  them should change. The code fix is trivial
  (`usize::try_from(n.max(0)).map_or(usize::MAX, |v| v)`, or a `match`). The
  `stdlib-strings` hunter reports the same pattern in `lua_std/basic.rs`,
  `vm/stack.rs`, `crmath/mod.rs`, `host.rs` and `main.rs`.
- `src/snapshot_format.rs` uses `#[cfg_attr(not(feature = "snapshot"),
  allow(dead_code))]`. Gating the constant, or its non-snapshot users, removes
  the need for it.

## X-002 - Process: the cost-cli scope brief placed the analysis in the wrong file

Raised by `cost-cli`. About the hunt brief, not the code.

The brief said `Program::analyze_cost` and `ScopeCost` live in
`src/cost_model.rs`. They live in `src/lib.rs` (`ScopeCost`, `CostAnalysis` and
all report rendering); `cost_model.rs` holds only the `Charge` registry,
`CostModel` and `Costing`. The hunter reviewed the correct code. A future hunt's
scope map should place the analysis in `lib.rs`, or in its own module if the
proposal in `notes/bugs-host.md` to move it out of the crate root lands.
