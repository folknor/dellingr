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

# Runtime defects: dispatch, VM core, tables

Filed from the 2026-10-23 defect hunt at a22d9c0 (scopes `dispatch`,
`vm-core`, `tables`, plus lateral notes from `codegen` and `strings`). The
hunters read the code statically and built or ran nothing, so every repro here
is traced by hand, not executed. The `tables` hunter asks that each repro become
a test before anyone acts on it.

## RT-015 - A terminal `__newindex` table step still takes a metamethod-depth unit

A residual of the RT-001 resolution (2026-10-23). The read-side rule is now
"a terminal step is free": an `__index` table that answers from its own
contents takes no depth unit, and only steps that continue the chain or call a
handler hold one. The write side was not covered by that ruling and still
charges a unit for every `__newindex` table step, terminal or not. So at the
depth limit `obj.k` through a one-level `__index` table succeeds while
`obj.k = v` through a one-level `__newindex` table raises
`MetamethodDepthExceeded`.

Since the register-native rewrite every store route (`.k`, `["k"]`, the
library's `set_table`) resolves `__newindex` in one loop,
`State::walk_newindex` in `src/vm/metamethod.rs`, which takes the unit before
classifying each handler; a ruling changes that one place.

**Direction.** Decide whether the write side follows the same rule. If it
does, apply it on every store route (`.k`, `["k"]`, host `set_table`) and pin it
with limit-edge tests like `tests/index_depth_routes.rs`; if it does not,
docs/limits.md's `MAX_METAMETHOD_DEPTH` wording should say why reads and writes
differ.
