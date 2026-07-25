# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #27, #28: recursive traversals abort the host on deep data

Two traversals over the same object graph, both recursive with no depth bound,
both reachable from ordinary script data rather than hostile input. One loop
because they are the same defect shape over the same graph - though see the
question below about whether they genuinely share an implementation.

### #27 (Medium, but verified as a hard abort) - GC mark phase is recursive

`GcHeap::mark` (`src/vm/object.rs:352-363`) -> `mark_children` (`:370-386`) ->
`Table::mark_values` (`src/vm/table.rs`) -> `Val::mark_reachable` ->
`GcHeap::mark`, recursing once per level of nesting with no bound. Roughly 5
frames per level.

`MAX_CALL_DEPTH` and `MAX_METAMETHOD_DEPTH` protect the interpreter; nothing
protects the collector.

**Verified by running:**

```lua
local t = {}
for i = 1, 500000 do t = { t } end
```

Result: `thread 'main' has overflowed its stack / fatal runtime error: stack
overflow, aborting / Aborted (core dumped)`. Exit code 134. A plain Lua script,
costing ~2 per iteration and well inside any normal budget, kills the host
process uncatchably. Auto-GC fires during the loop, so it aborts before the
script even finishes.

This is also what currently blocks the "no malformed save can abort during
load" guarantee from the previous loop: `materialize_payload` ends with a
`gc_collect()` (`save_state.rs:601`), and a decoded save may legitimately
contain a deep table graph. Rejecting deep graphs is not an option - scripts
build them normally - so the collector has to stop recursing.

### #28 (Medium) - `SaveBuilder::encode_object` is recursive

`src/vm/save_state.rs:313-351`: `encode_object` -> `encode_val` ->
`encode_object`, once per nesting level. A script builds the chain cheaply, the
host then calls `save_state()`, and the native stack overflows inside an API
whose signature promises `Result<_, SaveError>`. Script-controlled data killing
the host during a host API call.

In practice #27 usually fires first with auto-GC enabled, which is why the
save-side abort is less visible - but a host that disabled auto-GC, or that
saves a graph built up just under the collection threshold, reaches this one.

### Constraints

- **Determinism.** Save output must stay byte-identical, which means the
  traversal order of `encode_object` must not change. A worklist that visits
  children in a different order would silently change every save file. GC order
  is not externally visible, but arena/id assignment order in the save
  absolutely is.
- `mark` is on the GC hot path and carries `#[hotpath::measure]`. AGENTS.md
  notes the recursion caveat for that attribute.
- The heap uses colours (`Color::Unmarked` / `Reachable`); an explicit worklist
  needs to keep the "already visited" test doing the same work it does now, or
  marking gets quadratic on shared subgraphs.
- `optimizations.md` #5 already proposes the iterative worklist for both.

---

## Agreed implementation plan

Two separate implementations. They share a defect pattern, not useful code: GC
needs an infallible colour-transition tracer over `ObjectPtr`, while saving
needs a fallible deterministic continuation machine over four arenas plus
diagnostic paths. A generic walker would obscure both. They land together
because deep-save loading re-enters #27 through `gc_collect()`
(`save_state.rs:652`), but as two commits.

### Part 1: iterative GC tracer

Add a non-semantic `mark_worklist: Vec<ObjectPtr>` to `GcHeap`
(`src/vm/object.rs:189`). The codec builds a separate `SavePayload` and never
serializes `GcHeap`, so it is excluded automatically.

At collection start `std::mem::take` it into a local, clear defensively, mark
and drain, then restore the empty vector. No `RefCell`, no per-collection
allocation, capacity preserved across collections. There are no normal GC error
returns; on an invariant panic the heap keeps the empty replacement rather than
stale pointers.

Per-edge rule, O(1), no set or map:

- look the target up in the `SlotMap`;
- if `Unmarked`, set `Reachable` **immediately** and push;
- if already `Reachable`, do nothing.

Setting the colour *before* enqueueing is what makes cycles and shared
subgraphs enqueue each object at most once. Strings keep their existing direct
mark and never enter the object worklist.

`Markable` now enqueues object pointers and marks strings rather than tracing
children. The drain loop pops an object and enqueues: closed closure upvalue
values; table keys, values, and metatable. `mark_gc_roots` stays the single
authoritative root list. Touches `object.rs`, `table.rs:739`, `vm.rs:83`,
`anchor.rs:127`. Keep `#[hotpath::measure]` only on non-recursive entry points.

### Part 2: iterative save walker

**The order requirement is the whole risk here.** Current traversal, which the
byte layout encodes because ids are assigned at first encounter:

```text
globals in state.globals insertion order
  table id assigned before any child
    entry 0 key subtree     (fully traversed)
    entry 0 value subtree   (fully traversed)
    entry 1 key subtree
    entry 1 value subtree
    ...
    metatable subtree       (only after every entry)
  closure: bytecode first, then upvalues in vector order
```

Replace recursive `encode_object` and the object-reaching `encode_upvalue`
calls with a **LIFO task machine**, not a breadth-first discover-all-children
pass:

```rust
enum EncodeTask {
    Value { val, path, destination },
    Object { ptr, id, path },
    Upvalue { upvalue, path, destination },
}
```

Destinations are small index-based slots: root result, table key/value slot,
table metatable, closure upvalue slot, saved-upvalue value. Keep pending
object/upvalue slots as `Option`-based internals so successful completion
proves every field was filled, converting to `SavedObject`/`SavedVal` only at
the end. Checked indexed access, no production `unwrap`.

When expanding a table, push in this order:

1. metatable first, so it stays at the bottom;
2. entries in **reverse** index order;
3. within each entry, value first then key.

The next pop is then entry 0's key, and any object it discovers is pushed above
every sibling so its whole subtree completes before entry 0's value. Closure
upvalue tasks likewise pushed in reverse. That reproduces the recursive
preorder exactly.

Keep the existing `BTreeMap` id maps and assign ids at exactly the same
first-encounter points. **Leave `encode_bytecode` recursive and unchanged** -
it walks a different, syntax-bounded graph, and changing it at the same time
would enlarge the byte-stability risk for no benefit.

Do **not** carry full path strings in tasks: `format!("{path}...")` becomes
O(depth^2) memory once native recursion is gone. Use an append-only breadcrumb
arena:

```text
Breadcrumb { parent: Option<PathId>, segment: PathSegment }
PathSegment = Global(name) | TableKey(i) | TableValue(i) | Metatable | Upvalue(i)
```

Tasks carry only a `PathId`. Reconstruct the exact current text iteratively
only when producing `UnregisteredFunction`, preserving `.key[n]`, `[n]`,
`.metatable` and `.upvalue[n]` and the existing first-error order.

### Byte-stability gate

`tests/save_golden.rs` and `tests/fixtures/save_golden.bin` were captured from
the recursive implementation and committed before this work. If
`save_traversal_order_matches_golden_fixture` fails, the walker reordered the
graph - **fix the walker, do not regenerate the fixture.** Regeneration is an
`#[ignore]`d test precisely so a reordering cannot quietly rewrite its own
expectation.

### Benchmarks

Baselines captured on this machine immediately before the change:

| target | wall | warm avg |
|---|---:|---:|
| `alloc/closure` | 87.9 ms +- 1.5 | 281 us |
| `iter/pairs` | 84.3 ms +- 1.0 | 538 us |

`alloc/closure` is the meaningful gate: it collects ~10,000 times, which is
also why a fresh worklist allocation per collection would be a poor choice.
`iter/pairs` collects 7 times and should stay below noise.

### Tests

- `src/vm/object.rs`: a 100,000-object table chain marked and collected
  iteratively, verifying every reachable object survives.
- `src/vm/object.rs`: a cyclic/shared diamond covering object keys, values,
  metatables and closed closure upvalues - live objects survive, an unreachable
  cycle is swept.
- `src/vm/object.rs`: repeated collections leave the reused worklist empty
  between cycles.
- `src/vm/tests.rs`: an ordinary Lua chain with auto-GC enabled, deep enough to
  abort the old implementation, now completes.
- `tests/save_state.rs`: a 50,000-deep global chain with auto-GC disabled -
  save, load, and traverse every level iteratively.
- Exact diagnostic-path tests for an unregistered Rust function reached through
  a table key, a table value, a nested metatable table, and a closure upvalue.

### Recursion audit result

There is no third recursive traversal of the Lua object graph.
`Val::mark_reachable`, `Table::mark_values`, the slice/`IndexMap` `Markable`
impls, `Registry`, `TransientRoots` and `SuspendedEnvironment` are components
or root enumerators of the same GC walk; `encode_upvalue` is an edge adapter
inside the same save walk. Load materialization is already flat and two-pass.

This closes the unbounded-native-recursion class for runtime Lua data graphs.
Everything else that recurses is bounded: Lua calls at `MAX_CALL_DEPTH` 1000,
metamethod chains at 200, pattern matching at 200, table-valued `__call` chains
indirectly at 255 by the `u8` argument count, and the parser / `encode_bytecode`
/ `build_bytecode` family at `MAX_SYNTAX_DEPTH` 200 over a different graph.

### Superseded questions

1. Do #27 and #28 genuinely share an implementation, or only a pattern? The
   previous loop's reviewer argued the three recursive problems (this pair plus
   the now-fixed `build_bytecode`) share a design pattern but not useful
   generic code, since GC works on `ObjectPtr` reachability and colour
   transitions while save encoding needs allocate/fill phases plus
   deterministic paths and diagnostics. Confirm or correct that. If they do not
   share code, say whether they should still land together.
2. For #27: explicit gray stack (`Vec<ObjectPtr>`) is the obvious shape. Where
   exactly does the worklist live - a scratch `Vec` on `GcHeap` reused across
   collections, or allocated per collect? Reused avoids a per-GC allocation but
   adds a field that must not be serialized and must be cleared on error paths.
   What does it cost on the `iter/pairs` and `alloc/closure` benches?
3. For #28: what is the exact current traversal order, and how do you preserve
   it byte-for-byte under an explicit stack? This is the part I would most
   expect to get silently wrong - an explicit stack naturally reverses child
   order unless the children are pushed in reverse. Is there an existing
   byte-stability test that would catch a regression here, and is it strong
   enough?
4. Is there a third recursive traversal over the same graph that I have not
   listed? `Val::mark_reachable`, `Table::mark_values`, and the `Markable` impls
   are the obvious neighbours, and the previous loop added
   `TransientRoots`/`SuspendedEnvironment` impls. Sweep for anything else that
   recurses per nesting level.
5. Does anything else in the crate recurse per level of *script-controlled*
   data depth, outside these traversals? The parser is now bounded and
   `build_bytecode` is bounded; I want to know whether this closes the class or
   just two instances of it.

Read `src/vm/object.rs`, `src/vm/table.rs`'s `mark_values`, the `Markable`
impls, and `SaveBuilder` in `src/vm/save_state.rs`.
