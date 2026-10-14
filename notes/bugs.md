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

## BUG-012 - A host `State::call` on a cyclic `__call` table may not fire `on_error`

Reported by the fixer that guarded `__call` depth; confirmed by the reviewer.

A host calling a self-referential `__call` table directly through `State::call`
gets `MetamethodDepthExceeded` from the metamethod depth guard, but `call()`'s
only `host_error` site is in the RustFn branch, so the `__call` branch returns
the error without notifying `on_error`.

## BUG-013 - Integral negative-zero results now print `-0.0`, matching neither reference

Reported by the wave 2 reviewer; unverified against the diff corpus.

`number_to_string` renders `-0.0` as `"-0.0"` (Lua 5.4's float spelling). With
no integer subtype, results Lua 5.4 makes integers - `math.ceil(-0.5)`,
`0 * -1` on integers, `-x` for integer zero - print `0` in 5.4 and `-0` in 5.2,
while dellingr prints `-0.0`. Before the shared renderer, `tostring` printed
`-0` (5.2's answer). Decide which reference the renderer tracks for negative
zero; `-0` matches 5.2 everywhere and 5.4's float case is the only one it
misses.

## BUG-010 - Out-of-range argument errors are `RuntimeError`, not `ArgError`

Reported by the stdlib hunter; narrowed by a fixer.

`tonumber` (base out of range), `select` (index out of range) and `rawlen`
("table or string expected") now say the right thing, through a shared
formatter in lua_std.rs, but as `RuntimeError`: `ArgError` cannot carry a
custom detail. The fix is the `ArgDetail` enum from the hygiene entry on "bad
argument" owners, after which these become `ArgError` again.
