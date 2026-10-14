# bugs.md

Defects turned up by the hygiene hunt over the compiler, vm, surface, stdlib
and tooling scopes. Hunter reports, not verified findings: each may be wrong.

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

## BUG-010 - Out-of-range argument errors are `RuntimeError`, not `ArgError`

Reported by the stdlib hunter; narrowed by a fixer.

`tonumber` (base out of range), `select` (index out of range) and `rawlen`
("table or string expected") now say the right thing, through a shared
formatter in lua_std.rs, but as `RuntimeError`: `ArgError` cannot carry a
custom detail. The fix is the `ArgDetail` enum from the hygiene entry on "bad
argument" owners, after which these become `ArgError` again.
