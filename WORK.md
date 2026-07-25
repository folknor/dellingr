# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Target #24: `OP_CONCAT` allocates without bound at zero cost

**Verified.** This script builds a 1 MB string and reports `Cost used: 0`, and
still completes under `--limit 100`:

```lua
local s = "x"
for _ = 1, 20 do s = s .. s end   -- length 1048576, cost 0
```

Twenty more iterations is a terabyte attempt. `OP_CONCAT` charges nothing
(`src/vm/frame.rs:304-307`) while `concat_helper` does O(total bytes) of work
and allocation (`src/vm/eval.rs:228-263`), and the surrounding `while`/`for`
loop is deliberately free.

The README's Budget section concedes structural freebies, but that argument is
about *time* spent on control flow. Unbounded *allocation* is a different
failure domain: the host process is OOM-killed rather than idling through a
tick budget. A game host cannot defend against this by setting a smaller cost
budget, because the cost is zero at any budget.

### The scope question this loop has to answer first

There are two candidate fixes and they are **not** equivalent:

1. **Charge concat per output byte.** This is a cost-model change: it makes
   existing scripts measure differently, with no conversion formula. That is
   exactly what #16 covers and #16 is deliberately deferred as a release
   decision. Doing it here would smuggle a breaking cost change into a bug fix.
2. **Cap the maximum string length.** This is a *resource limit*, like
   `MAX_STACK_SIZE = 1_000_000` or `MAX_CALL_DEPTH = 1000`. It changes no
   measured cost for any script that stays under the cap, and turns the OOM
   into a clean catchable error.

**Option 2 is the one this loop should take**, unless the reviewer can show it
does not actually close the hole. Option 1 stays with #16.

### Constraints

- **Do not change what any existing script costs.** If a script's `Cost used`
  changes, the fix is out of scope.
- Errors must leave the State consistent and reusable, and the failure has to
  be a normal error, not a panic or an abort.
- Determinism: the cap must be a fixed constant, not memory-pressure sensitive.
- `unwrap_used` denied outside `#[cfg(test)]`; clippy denies warnings.

---

## Agreed implementation plan

The reviewer independently agreed with the scope decision: a fixed length cap
is a resource limit like `MAX_CALL_DEPTH`, adds no charge, and leaves every
below-cap script's `Cost used` byte-identical. Per-byte charging stays with
#16.

**Qualification to keep honest:** a per-string cap closes #24, it is *not* a
total-memory quota. Many below-cap strings still add up. A total-heap or
snapshot-size quota is a separate resource dimension and needs its own
constant - do not let this silently become one.

### The limit

```rust
pub const MAX_STRING_BYTES: usize = 16 * 1024 * 1024;
```

Inclusive: `MAX_STRING_BYTES` succeeds, `+1` fails. Sixteen times the
demonstrated 1 MiB script, bounds construction peaks to tens of MiB even when
the producer buffer and interned copy coexist, stops exponential concat at
32 MiB instead of a terabyte, and is fixed across hosts so replay stays
deterministic. **One limit for every Lua string and every temporary intended to
become one** - concat, format and gsub do not get different limits.

### Two layers, because interning alone is too late

1. Every string admitted to the interner satisfies the cap - the final funnel
   is `GcHeap::alloc_string` (`object.rs:326`), which already hashes every byte
   (`567`), so one O(1) branch before GC/hash/copy is negligible.
2. **Expanding producers check before growing their buffer.** Checking only at
   intern time is too late: the dangerous `Vec` already exists.

Add a shared checked-growth helper doing `checked_add` against the cap before
any `reserve` / `extend_from_slice` / `push` / `resize`.

### Producers that must enforce it

| path | required work |
|---|---|
| `..` | fold into the existing first length pass (`eval.rs:238-266`). **`total_len +=` at `240` can itself overflow** - use checked arithmetic. Reject before the stack truncation at `266`. |
| `table.concat` | checked append inside the loop (`table.rs:177`) |
| `string.format` | outer output (`string_format.rs:68`) plus a preflight for `%q`, which builds an expanded temporary first (`775`) |
| `string.gsub` | all three branches (`string.rs:491`, `511`, `544`) - replacements, captures, function returns and the unchanged tail |
| `gmatch` | the leading-`^` rewrite produces `pattern.len() + 1` (`string.rs:423`) |
| `print` | assembles every argument into one `String` (`basic.rs:129`); 255 near-cap arguments request gigabytes. Cap the assembled message. |
| host `push_bytes` / `push_string` | **must become fallible** (`stack.rs:101`), or a host can inject an over-cap string and invalidate every non-expansion argument |
| compiled literals | `find_or_add_string_bytes` (`parser.rs:279`) and escaped-literal decoding, which reserves the raw source length (`306`); frame creation bypasses `State::alloc_string` and calls the heap directly (`eval.rs:471`) |

Non-expanding operations - `sub`, `upper`, `lower`, `reverse`, capture results -
need no producer check once the admission invariant holds; the backstop covers
them.

### Error shape

Dedicated variant, not `RuntimeError`:

```rust
ErrorKind::StringSizeExceeded { size: usize, limit: usize }
```

rendering `string size 16777217 exceeds limit 16777216`. `RuntimeError` would
force hosts to parse text to recognise a resource-limit termination, and every
other resource failure already has its own variant (`BudgetExceeded`,
`CallDepthExceeded`, `StackOverflow`). Report the first invalid size; an
arithmetic overflow reports `usize::MAX`. Add the matching
`LoadError::StringSizeExceeded`.

Not recoverable inside Lua - `pcall` is deliberately absent - but catchable by
the host through `Result`. Rejection must happen before any mutation, so the
State stays reusable and the host-visible stack is unchanged.

### Snapshots

The load path enforces nothing today: strings decode through unrestricted
`read_bytes` (`save_state.rs:1472`, used at `1521` and `1739`), materialization
bypasses `State::alloc_string` (`1008`), and bytecode literals restore without
validation (`1285`). Apply **exactly the same inclusive boundary** at load, on
both `payload.strings` and every saved bytecode `string_literal`, rejecting
before copying. Do **not** apply it to whole snapshots or to metadata such as
source names and registered function ids. The wire format is unchanged, but a
v5 snapshot holding an oversized string now fails to load - document that.

### Tests

The 1 MiB repro still succeeds under `--limit 100` with `Cost used: 0`; exactly
16 MiB succeeds and 16 MiB + 1 returns the dedicated error; every producer in
the table above; `string.sub` of a cap-sized string; a failed operation leaves
the State reusable and the host stack unchanged; **existing below-cap costs are
byte-identical**; snapshot round trip at exactly the cap succeeds while forged
cap-plus-one runtime and bytecode strings fail before materialization.

**Public API break:** `push_bytes` and `push_string` return `Result<()>`. This
is the third in the run, after `State::to_string` taking `&mut self` and
`set_top`/`pop` becoming fallible. Note it lands adjacent to #62, which wants
the same methods to enforce `MAX_STACK_SIZE` - that fix gets cheaper once these
are already fallible.

### Superseded open questions

1. **Is a length cap sufficient?** Concat is not the only way to build a large
   string. Enumerate every path that can produce an arbitrarily large string
   or byte buffer - `..`, `table.concat`, `string.format`, `gsub` output,
   `string.sub`, host `push_bytes` - and say which must enforce the cap for it
   to be a real defence rather than a speed bump. `string.rep` is absent, which
   removes the obvious one.
2. **What is the right limit, and is it one constant or several?** Reference
   Lua has no equivalent, so this is a product decision: pick a number that no
   plausible game script reaches but that bounds memory usefully, and justify
   it. Consider that the VM already allows a 1M-value stack.
3. **Where is the check cheapest?** Ideally one place that every large-string
   producer already funnels through - is there such a point (`alloc_string`, or
   the interner), or does each site need its own check? Measure or argue the
   cost on the hot path, since concat of short strings is common.
4. **Error shape.** Which `ErrorKind`? `RuntimeError` seems right, but check
   whether a dedicated variant is warranted so hosts can distinguish "script
   asked for too much memory" from other runtime faults - the same reasoning
   that produced `ScriptError` in the taxonomy loop.
5. **Does the cap interact with snapshots?** A saved state could contain a
   string at or near the cap, and a load must not reject a state the running VM
   accepted. Confirm the load path agrees with the runtime limit.

Read `src/vm/frame.rs` (`OP_CONCAT` dispatch), `src/vm/eval.rs`
(`concat_helper`), `src/vm.rs` (`alloc_string`, the existing MAX_* constants),
`src/lua_std/table.rs` (`table.concat`), `src/lua_std/string.rs`, and
`src/vm/object.rs` (the string interner).
