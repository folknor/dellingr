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

# Standard library defects: base, table, math, string, patterns

Filed from the 2026-10-23 defect hunt at a22d9c0 (scopes `stdlib` and
`strings`). The hunters read the code
statically and built or ran nothing. The `stdlib` hunter quotes reference
behaviour from the 5.2.4 and 5.4 C sources (`lbaselib.c`, `lmathlib.c`,
`ltablib.c`), not from running the interpreters.

For most of the stdlib findings the `stdlib` hunter's yardstick is
docs/compatibility.md's opening claim: "Every deliberate divergence from Lua 5.2
and 5.4. A difference not listed here is a bug. Features that are absent
altogether are listed in the README under 'Won't implement'." Where a finding is
only a doc gap, the fix can be a code change or a new line in that file, but as
things stand the claim is false.

The `strings` hunter reports that the pattern engine, checked step by step
against 5.4 `lstrlib.c`, and `string.format`, checked against 5.4's per-conversion
flag and precision tables, gave no wrong-result defect.

## LIB-002 - Default `table.sort` may claim comparison errors raised inside `__index` handlers

A lateral from the RT-007 resolution (2026-10-23). Read from the code, not yet
confirmed by a test.

**Mechanism.** `table_sort_result` (table.rs) tags every
`TypeError::Comparison` error with `LibraryFunction::Sort` when no comparator
was given. A default sort over a proxy table reads its elements through
`__index`, so a comparison error raised by Lua code inside that handler (for
example `if a < b` on mismatched types) would come back labelled as sort's own
comparison failure, which points the script author at the wrong code.

**Host-visible split.** `on_error` reports the error before the tag is added,
so the notification and the error returned from `call()` differ in that
attribution.

**Direction.** First write a test that confirms it. If it holds, tag only the
comparisons sort itself performs (at the comparison site, not on every error
leaving the sort) and leave errors from nested handlers untouched.
