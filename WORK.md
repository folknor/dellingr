# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #26, #32, #48: the table-operations cluster

Three defects in `table.insert` and `table.sort`. All verified by execution;
5.2 and 5.4 agree throughout.

| case | dellingr | Lua 5.2 | Lua 5.4 |
|---|---|---|---|
| `t={10,20,30}; table.insert(t,1,99)`, then `pairs` | `4=30 3=20 2=10 1=99` | `1=99 2=10 3=20 4=30` | same as 5.2 |
| `table.sort{1,"a",2}` | succeeds, `1 2 a` | error `attempt to compare string with number` | same |
| `table.sort{{},{}}` | succeeds (no-op) | error `attempt to compare two table values` | same |
| `table.sort{true,false}` | succeeds (no-op) | - | error `attempt to compare two boolean values` |

### #26 (Medium) - `table.insert(t, pos, v)` reverses order and is O(N^2)

`src/vm/table.rs:577-601` (`Table::array_insert`). The shift loop runs high to
low doing `shift_remove(key i)` then `insert(key i+1)`. Each re-insert *appends*
at the end of the `IndexMap`, so the shifted keys end up in reverse insertion
order followed by the new key.

Two consequences, of unequal weight:

- **Cost (the real defect).** Every `shift_remove` is O(tail) in an `IndexMap`
  and there are O(N) of them, so a single `table.insert(t, 1, v)` on a 50k
  array is on the order of 10^9 memmoves - charged 1. This extends the
  "O(N) shifts charged as 1" entry that `OPTIMIZATIONS.md` already tracks; that
  entry understates it as O(N).
- **Order (weaker claim, be honest about it).** Lua does **not** specify `pairs`
  iteration order, so the reversal is not strictly a conformance violation.
  It is still a surprising, user-visible regression from insertion order, and
  the fix below restores it for free. Do not describe this as a conformance fix
  in comments or the commit message.

`array_remove` happens to preserve order because its forward loop re-appends in
ascending key order - so only the insert path is wrong.

**Cache nit, same function:** `array_insert` unconditionally sets
`cached_array_len = Some(len + 1)` (line 599). For a non-sequence such as
`t={1,2,3}; t[5]=5`, after the shift `t[5]` is non-nil, so `len + 1` is not a
border. Note the measured `#u` divergence here (dellingr 4, reference 5) is
**not** a conformance target - `#` on a table with holes is explicitly any
border, and both answers are valid. Fix the cache because storing a value that
is not a border at all is wrong on its own terms, not to match reference:
only trust `Some(len + 1)` when `get(len + 2)` is nil, else `set(None)`.

### #32 (Medium) - `table.sort` with a comparator is O(N^2) comparator calls

`src/vm/table_ops.rs:305-329`. Bubble sort with no early-exit swap flag, so it
always runs the full N(N-1)/2 rounds even on already-sorted input. The
comparator call is free by design and a `return a < b` body charges ~0, so
`table.sort` on 10k elements is ~5*10^7 full call-machinery round trips charged
`n` = 10^4.

The charge itself is correct and must stay where it is: `consume_cost(n.max(1))`
runs at line 299 **before** any comparator runs, which is the L18 contract and
is covered by an existing test. Do not move or rescale it in this loop -
changing what sort costs is a cost-model change, and cost-model changes are #16.

### #48 (Low) - default `table.sort` silently orders incomparable types

`src/vm/table_ops.rs:330-349`. Without a comparator, numbers sort before
strings and anything else compares `Equal`, so `table.sort{1,"a",2}` succeeds
and `table.sort{{},{}}` is a no-op "sorted". Reference raises a type error in
every one of those cases. Given the project's "errors kill the callback" stance,
silently succeeding is the divergence.

The blocker is structural: `arr.sort_by` takes an infallible comparator, so the
error has nowhere to go. That is why this needs the same rewrite as #32.

### Why one loop

`optimizations.md` #12 is the standing sort rewrite and #11 the insert
rewrite. #32 and #48 are the same function and #48 cannot be fixed without
making the sort fallible, so splitting them would mean writing the comparator
plumbing twice.

### Constraints

- **Determinism is a product requirement.** The sort must be deterministic for
  equal elements and identical across hosts. Reference `table.sort` is *not*
  stable and dellingr need not be either, but it must be reproducible.
- A comparator can re-enter Lua, mutate the table, error, or exhaust the
  budget. Today the sort works on `arr`, a detached copy, with
  `with_rooted_values` keeping elements alive; preserve that GC discipline.
- Reference detects an inconsistent comparator ("invalid order function for
  sorting"). Decide deliberately whether to reproduce that or to remain
  well-defined-but-silent, and record the choice.
- `unwrap_used` denied outside `#[cfg(test)]`; `HashMap`/`HashSet` banned.
- Charge nothing new and rescale nothing (#16 owns cost-model changes).

---

## Agreed implementation plan

The orchestrator and the deep reviewer converged independently on carried-value
rotation for insert and a hand-written fallible heap sort. Settled, not a menu.

### `Table::array_insert` - carried-value rotation

```text
carry = new value
for key in pos..=len:
    old = get(key)
    write carry at key
    carry = old
write carry at len + 1
```

Use `Table::insert` for the writes so missing keys and nil values keep normal
table semantics. **Do not remove and reinsert live keys** - that is the whole
bug. An existing-key write keeps its slot, so order is preserved and each step
is an O(1) lookup instead of an O(tail) memmove: O(N) total.

Storage shapes all work without special-casing:

- `Inline` (`table.rs:456-470`) updates existing keys in place, and promotion at
  the fifth entry preserves order (`479-491`).
- `Map` (`472-474`): `IndexMap::insert` updates an existing value without moving
  its entry.
- Tombstones: keep the initial `compact_dead()`. A nil carried through the
  rotation then goes through the established remove/tombstone paths
  (`358-361`, `512-543`).

**Drop the `ensure_map()` call** - rotation does not need it. `array_remove`
still does, and is unchanged.

For the sparse case `t={1,2,3}; t[5]=5; table.insert(t,1,v)`, `array_len()`
picks border 3, and rotation yields `t[1]=v, t[2]=1, t[3]=2, t[4]=3, t[5]=5`.
Key 5 keeps its insertion-order slot and the new key 4 is appended after it -
deterministic, though not ascending `pairs` order. That is fine; `pairs` order
is not a conformance requirement.

**Cache:** set `Some(len + 1)` only when the inserted value is non-nil **and**
`get(len + 2)` is nil; otherwise `None`. In the sparse case `t[5]` is non-nil,
so border 4 must not be cached. The reader (`299-312`) trusts any `Some`
without validating it, which is why storing a non-border is a real defect.

### `table_sort` - one fallible iterative heap sort for both branches

Hand-written iterative min-heap sort followed by a final reverse. Rationale:

- O(N log N) worst case, in-place over `arr`, no recursion, no host-dependent
  pivot choice.
- **Every index comes from fixed heap bounds, so a comparator that lies about
  ordering cannot produce an out-of-bounds access.** This is the load-bearing
  reason not to use `sort_by`: it is infallible, cannot short-circuit on the
  first error, and assumes a total order that an arbitrary Lua comparator need
  not provide.
- The comparison helper returns `Result<bool>` so errors propagate with `?`.

GC and re-entrancy invariants to preserve exactly:

- Extract `arr` and release the table borrow before any callback (`285-292`).
- **Root the complete initial array for the whole sort.** `Vec<Val>` is
  invisible to GC and `transient_roots` is part of the authoritative root set
  (`vm.rs:90`). Rooting only the current comparison pair breaks the forced-GC
  test at `src/vm/tests.rs:420`, whose comparator clears the source table
  before collection.
- Hold no `&Table`, `&GcHeap` or element reference across `State::call`; copy
  each `Val`, then call.
- On comparator error, return **before** `set_array`. The detached array may be
  partially permuted and is simply dropped, so comparator-authored table
  mutations survive and sort-authored ones do not.
- On success, re-fetch the table and write back as today (`351-360`).

Keep `consume_cost(n.max(1))` exactly where it is (`294`), before comparator
selection and sorting. The L18 pre-mutation contract is covered by
`tests/error_handling.rs:577`.

**Inconsistent comparators: deliberately deterministic-but-silent.** Heap sort
always terminates safely even for `return true`. Reference's "invalid order
function for sorting" is an incidental artifact of its quicksort bounds check;
reproducing it would mean adding algorithm-specific extra comparator calls.
Record this in a code comment and pin it with a test.

### Default comparison (#48)

Put the check **inside the fallible comparator**, not in a pre-scan. Reference
permits a singleton incomparable value - both 5.2 and 5.4 accept
`table.sort{true}` because no comparison ever happens - so a pre-scan rejects
too early.

Implement primitive `<` consistently with `eval_compare`
(`src/vm/eval_store.rs:476`): numbers numerically, strings lexicographically,
every other pairing a `TypeError::Comparison`. Use `Val::typ(&heap)`, **not**
`typ_simple`, which labels every object a table (`lua_val.rs:86` - that is
finding #51).

Both references compare the second element against the first for `{1,"a"}`,
report `attempt to compare string with number`, and leave the table unchanged.
A min-heap's first child-versus-parent comparison reproduces that pairing and
errors before any writeback.

`TypeError::Comparison` formatting (`src/error.rs:371`) currently produces
"attempt to compare table with table". Special-case equal types to get
reference's "attempt to compare two table values".

### Tests

Insert: inline, map and tombstone paths; the sparse cache case; that `pairs`
order after `table.insert(t,1,v)` is ascending again.

Sort: mixed number/string, two tables, and two booleans all error with
reference's exact message; `table.sort{true}` succeeds; comparator-call count
is O(N log N) rather than N(N-1)/2; a comparator that mutates the same table;
nested sort re-entry; an inconsistent comparator terminates deterministically;
and the table is unchanged after a default-comparison error.

Preserve the existing GC/quiescence tests at `src/vm/tests.rs:420` and
`tests/save_state.rs:216`.

Read `src/vm/table.rs` (`array_insert`, `array_remove`, storage shapes),
`src/vm/table_ops.rs` (`table_sort`), `src/vm/eval_store.rs` (`eval_compare`),
`src/error.rs` (`TypeError::Comparison`), and `src/lua_std/table.rs`.
