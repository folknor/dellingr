# bugs.md

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

Defects turned up by the 2026-11-17 performance hunt (codex `write-sol-high`,
one hunter per scope). Unverified: the fix pass decides which are real.

---

## BUG-006 - Method-cache mutation tests do not exercise a warmed entry

Raised by the lookup hunter. The two method-cache mutation tests in
`tests/integration/runtime/field_cache_mutation.rs` use separate `t:f()` sites
before and after the mutation. They verify results, but do not exercise
revalidation of an entry that was warmed at the same site. Other route tests
give overlapping coverage.

## BUG-007 - The cache differential compares less than the observable contract

Raised by the lookup hunter. `src/vm/cache_differential.rs` compares printed
output, total cost and rendered errors. It discards return values and does not
compare snapshot bytes or subsequent state behaviour, so a cache that corrupts
only those would pass it.

## BUG-008 - Anchor drops from another thread can change collection survival

Raised by the host-boundary hunter. Anchor drops queue their key under a mutex
for the State to drain; a cross-thread drop concurrent with execution can
change which objects survive a collection, a determinism hazard. A replacement
anchor design must handle release timing deliberately.
