# Cross-cutting findings: repo rules

Filed from the 2026-11-04 defect hunt. Findings that belong to no single scope.
Hunt reports are work documents and may be wrong; verify before acting.

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

Raised by `stdlib-strings` and `snapshot`. Awaiting a decision by the project
owner.

### The two rules

- AGENTS.md: "Never disable a lint; fix the code. The `clippy.toml` bans name
  their replacements."
- `clippy.toml` reason texts invite a scoped `allow` in several bans:
  `Result::unwrap_or` and `Result::unwrap_or_default` ("or use a scoped allow
  for an intentional fallback"), `std::panic::set_hook` and `take_hook` ("Use a
  scoped allow only for a permanent filtering installer"), and `std::io::stdout`
  and `stderr` ("Hosts and harnesses whose job is printing use a scoped allow").

### What is in the tree

The hunters named `saturating_usize` in `src/lua_std/string.rs`
(`#[allow(clippy::disallowed_methods)]` for `Result::unwrap_or`) and
`#[cfg_attr(not(feature = "snapshot"), allow(dead_code))]` in
`src/snapshot_format.rs`. A scan of `src/` during triage found at least 24
suppressions across several lints, most with an inline reason:

- `clippy::disallowed_methods`: `src/main.rs` (four), `src/host.rs`,
  `src/vm/stack.rs`, `src/lua_std/basic.rs`, `src/lua_std/string.rs`,
  `src/crmath/mod.rs`
- `clippy::let_underscore_must_use`: `src/main.rs`, `src/host.rs`
- `clippy::expect_used`: `src/compiler/lexer.rs` (two),
  `src/compiler/parser/state.rs` (two)
- `clippy::panic`: `src/compiler/lexer.rs`
- `clippy::float_cmp`: `src/lua_std/math.rs`, `src/vm/table.rs` (two), and a
  module-wide `#![allow]` in `src/crmath/mod.rs`
- `clippy::print_stderr` and one multi-lint `allow`: `src/crmath/tests/golden.rs`
- one multi-lint `allow` in `src/vm/object.rs`
- `dead_code` (feature-conditional): `src/snapshot_format.rs`

`tests/`, `examples/` and `scripts/` were not scanned.

### Directions raised so far (not exhaustive)

- Align on AGENTS.md: reword the `clippy.toml` reasons that invite a scoped
  `allow`, and replace each suppression with code the lints accept (the
  `stdlib-strings` hunter suggested forms such as
  `usize::try_from(n.max(0)).map_or(usize::MAX, |v| v)` or a `match` for the
  `unwrap_or` sites; gating the constant or its non-snapshot users for the
  `dead_code` one).
- Align on `clippy.toml`: state in AGENTS.md which scoped suppressions are
  accepted and on what terms (for example the cases the ban reasons name, with
  an inline reason), and leave conforming sites as they are.

A point noted during triage: rewriting `unwrap_or(x)` as an equivalent
`map_or` call satisfies the ban mechanically without changing what the code
does.
