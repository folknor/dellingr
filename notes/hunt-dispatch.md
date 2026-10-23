# Defect hunt: dispatch

Scope: src/vm/eval.rs, eval_control.rs, eval_index.rs, eval_store.rs,
frame.rs, registers.rs, stack.rs, caches.rs, src/cost_meter.rs, followed into
metamethod.rs, global_env.rs, table.rs and the verifier where dispatch relies
on them.

Summary: the call/return machinery, open-results windows, unwind ownership,
chain-charge release, cost batching and the GC/cache weak processing all hold
up. I checked the batching arithmetic against a unit-at-a-time meter, every
route from the loop to a Rust callback or a nested invocation for a prior
flush, and every stack growth for a preflight or a prior reservation. The
real problems are in how the register fast paths and the older stack-protocol
helpers beneath them split the work. The two layers do not demand the same
resources: one counts metamethod depth and needs stack scratch, the other
does neither. Which layer runs depends on inline-cache history, and that
history is not part of a snapshot.

---

## F1. Whether an instruction succeeds depends on inline-cache history and syntax (replay divergence)

Severity: medium. It breaks determinism, but only at a limit edge.

### What happens

For `obj.k` / `obj:k()` on a table whose raw lookup misses and whose
metatable `__index` is a table, `reg_get_field` has two routes:

- **Direct route** (`get_field_direct` -> `get_index_table_field_direct`).
  It answers from the `__index` table without temporaries, without
  `with_metamethod_depth` and without any stack check.
- **Slow route** (`get_field_slow` -> `instr_get_field` ->
  `get_table_with_key` -> `handle_index_metamethod`). It needs one slot above
  the frame top (`check_stack_space(1)` in `get_field_slow`) and enters
  `with_metamethod_depth`. That refuses with `MetamethodDepthExceeded` when
  `metamethod_depth >= MAX_METAMETHOD_DEPTH`.

The choice between them is made by cache state.
`get_index_table_field_direct` begins with:

```rust
if cache
    .and_then(FieldLookupCacheSlot::get_method)
    .is_some_and(|entry| entry.method_index.is_none())
{
    return None;
}
```

That check runs before any validation, and nothing below it ever overwrites
the entry, so a "miss" entry is permanent for the site. The miss entry is
written whenever the site once saw any of these:

- a receiver whose `__index` is a function;
- a receiver whose `__index` is a non-table;
- a receiver whose `__index` table lacked the key, which is ordinary two-level
  inheritance: `Derived.__index = Derived`, `setmetatable(Derived, {__index = Base})`,
  calling a `Base` method on a `Derived` instance.

From then on, every receiver at that site takes the slow route, including
receivers whose method is one level up. With a cold cache (a fresh closure
runtime, or any State after `load_state`, because caches "are never
serialized and are rebuilt cold when a snapshot is loaded", caches.rs), the
same receiver takes the direct route.

So at either limit edge, the same instruction on the same receiver succeeds
or fails depending on which receivers the site saw earlier:

- **Metamethod depth at the limit.** A script reaches this without native
  recursion. Lua-to-Lua recursion through a non-function `__call` chain holds
  one chain unit per level: `obj()` where `obj`'s `__call` is a table whose
  `__call` is a Lua function. `begin_callable_call` charges the unit and hands
  it to the admitted record, so 200 levels (`MAX_METAMETHOD_DEPTH`, well under
  `MAX_CALL_DEPTH` = 1000) sit at the limit with native depth at 1. There,
  `self.name` on a one-level class instance:
  - works with a cold cache;
  - raises `MetamethodDepthExceeded` with a warm "miss" cache.
- **Stack cap.** Admission reserves whole frames. `max_stack` is a u16, and
  call arguments nested in call arguments drive it into the hundreds or
  thousands, so `MAX_CALL_DEPTH` frames can reach `MAX_STACK_SIZE` from script
  alone (a host can also push there). With the frame top at the cap, the same
  `obj.k` succeeds through the direct route and raises `StackOverflow` through
  the slow route.

A continuous run and a run resumed from a snapshot of the same logical state
therefore diverge: one kills the callback, the other continues.

The same layering also makes the outcome depend on syntax, with no caches
involved. `reg_get_table` has no direct `__index` route: its fast path covers
only a raw hit or a table without a metatable. At the depth limit:

- `obj.k` succeeds;
- `obj["k"]` raises `MetamethodDepthExceeded`.

String receivers are a third variant. `index_string`'s first step into the
string library table takes no depth unit at all, cached or not.

### Claims broken

- docs/determinism.md: "The same script and inputs produce the same result
  and the same cost on a given build." A save/load in between must not change
  results. That is the replay contract, and the reason caches are kept out of
  snapshots.
- caches.rs `RuntimeCaches` doc. It presents caches as an unobservable
  acceleration: rebuilt cold on load, cleared on collection, with "a hit
  trusts a matching handle". Being cold is observable here.
- docs/limits.md `MAX_METAMETHOD_DEPTH`: "active steps in an index, newindex
  or non-function call chain". Whether a one-step `__index` lookup counts
  depends on whether the site was written `.k` or `["k"]`, and on cache
  history.

### Direction

Decide what one non-nesting `__index` table step costs against the depth
limit, and make every route implement that one rule. Two options:

- **Never count it.** Only steps that recurse, a second table level or a
  handler call, take a unit.
- **Always count it.** The direct route then checks the limit too.

The first option matches what the direct route does today and needs no
stack. Separately, the sticky "miss" gate in `get_index_table_field_direct`
should not decide the route. Either re-resolve on a miss entry, or drop the
early return and let `get_cached_index_table_field`'s validation handle it.
The slow route's stack scratch is covered by F2.

Structurally: the slow paths exist only to reuse the stack-protocol helpers
(`instr_get_field`, `instr_set_field`, `instr_get_table`, `instr_length`).
Those helpers repeat the fast-path lookups and need scratch for operands that
are already rooted in registers. Rewriting them register-native, with
operands by value and results written to `dst`, and with stack use only for
an actual handler call window, removes this whole class of route-dependent
behaviour. It also removes the double lookup on every slow-path entry: see L1.

---

## F2. "Its bytecode never checks it again" is false: register ops still fail `StackOverflow` mid-body

Severity: low to medium. A contract statement is wrong, and the behaviour is
inconsistent.

`admit_lua_call` says:

> The frame reserves every register it can use, parameters, locals and
> temporaries, against the stack cap now, so its bytecode never checks it
> again.

docs/limits.md frames the cap the same way: a call "fails there with
`StackOverflow`, before its body runs".

In fact, several plain register instructions still grow the stack above the
frame top and check the cap. None of them is a metamethod path:

- `OP_CLOSURE`: `reg_closure` -> `instr_closure` -> `push_closure`, which
  does `check_stack_space(1)`, then pushes and pops into `dst`.
- `OP_NEW_TABLE_TEMPLATE`: `new_table_with_template` does
  `check_stack_space(1)`. Meanwhile `OP_NEW_TABLE` and
  `OP_NEW_TABLE_PRESIZED` allocate straight into the register and never
  check. So at the cap `{}` works and `{x = 1}` raises `StackOverflow`.
- `OP_CONCAT`: `reg_concat` does `check_stack_space(count)` and copies every
  operand above the frame, then concatenates there. So `a .. b .. c` needs 3
  free slots above a frame that already owns `a`, `b` and `c`.
- Every slow path (`get_field_slow` 1, `get_table_slow` 2, `set_field_slow` 2,
  `set_table_slow` 3, `len_slow` 1) needs scratch, plus 3 or 4 more for a
  handler call. Through F1 this need is cache-dependent.

A frame whose top sits within a few slots of `MAX_STACK_SIZE` is admitted.
It then dies partway through its body on a closure, a template constructor or
a concatenation. Where it dies depends on which of these instructions it
reaches first.

A side note on `OP_CLOSURE`: `instr_closure` calls `find_or_create_upvalue`,
which inserts into `open_upvalues` and allocates pool slots, before
`push_closure`'s preflight refuses. The rejected instruction is therefore not
side-effect free. It is harmless only because the error always kills the
frame, and unwinding closes those upvalues.

### Direction

Make the claim true rather than reword it:

- allocate the closure and the template table directly into `dst`, as
  `alloc_table` already does;
- concatenate from the register range in place (`concat_helper` only needs a
  slice);
- reserve a fixed handler headroom at admission, the largest call window a
  slow path opens (4), so that only open-results windows and Rust callbacks
  can push past a frame's reservation.

Admission then fails up front, as limits.md describes. If the headroom is not
wanted, fix the comment and limits.md to list the instructions that still
check.

---

## F3. A Lua-to-Lua admission `StackOverflow` is located inside a function that never ran

Severity: low. The message points the script author at the wrong place.

`admit_lua_call` refuses two ways, and the two report different locations.

**`CallDepthExceeded`** returns before the record is pushed. The error
reaches the caller's unwind and is located at the caller's `OP_CALL` line.

**`StackOverflow`** (frame does not fit) goes through `reject_admission`,
which does three things:

- it builds the callee's frame with `ip = 0` (`admitted_frame()`);
- it calls `report_frame_error` on it, so the error gets the callee's
  `line_info[0]` as its line;
- it gives the error a traceback whose innermost frame is the callee, then
  reports to the host.

`unwind_invocation` then keeps that location and traceback, because the trace
is already non-empty.

So the script author sees the overflow inside the body of a function that
docs/limits.md says fails "before its body runs". The traceback even has a
line for it. Two refusals at the same call site, one per limit, point at
different places.

### Direction

Return admission errors unreported and unlocated, as `CallDepthExceeded`
does, and let the caller's unwind locate them at the call instruction. With
nothing left to report, `reject_admission` reduces to the bookkeeping undo.

---

## F4. Numeric `for` control errors claim arithmetic

Severity: low.

`instr_for_prep` and `instr_for_loop` report a non-number start, limit or step
as `TypeError::Arithmetic`. That renders as "attempt to perform arithmetic on
a nil value". `for i = 1, n do` with `n` nil performs no arithmetic the
author wrote, so the message names the wrong operation. This is a misleading
message by AGENTS.md's own standard. It is not a request to match reference
wording.

`instr_for_loop` re-validates the three hidden control registers on every
iteration. The script cannot write those registers, so that check is dead
work on the hottest loop opcode.

### Direction

Add a `TypeError` variant naming the `for` field (initial value, limit,
step). Drop the per-iteration re-checks, or make them `debug_assert`s.

---

## F5. The method IC's `globals_version` guard has a rationale that does not hold

Severity: low. The comment is wrong, and the guard costs performance.

`get_cached_index_table_field` rejects an entry when `globals_version` moved.
The comment says that otherwise "a pre-warmed callsite resurrects the
pre-swap binding inside the sandbox" when `mt.__index = string`.

The cached route, however, re-validates everything that uncached resolution
reads:

- the receiver's metatable identity;
- the `__index` slot's key, and its value equal to the cached handler;
- the method table's version, or the key at the cached index.

None of that reads globals. `mt.__index = string` stores the table value, so
after a `with_restricted_env` swap, uncached resolution also goes through
that same table. A cached hit cannot return anything that a cold lookup
would not.

The guard only invalidates. `write_builtin_slot` bumps the version on every
builtin write, so a script assigning to any builtin name (`print = myprint`
in a loop, say) dumps every method IC and every global cache entry. Global
caches never cache builtins (`GlobalEnv::read` returns before populating),
so they do not need that bump either.

### Direction

Remove the field and the bump on builtin writes, or document the actual
hazard if there is one I missed. In that case the comment needs to name it.

---

## Lateral findings (performance and smells, not contract breaks)

- **L1.** Every slow-path entry repeats the fast path.
  - `get_field_slow` -> `instr_get_field` re-runs `get_table_field_direct`
    and `get_index_table_field_direct`, both of which just failed in
    `get_field_direct`.
  - `set_field_slow` -> `instr_set_field` re-runs `try_set_field_cached` and
    `try_set_field_direct`.
  - So every `__index` function or two-level chain read pays the lookups
    twice, plus the push/pop of a copy of an already-rooted register. This is
    the same layering as F1 and F2.
- **L2.** The sticky method-IC miss is tracked in notes/optimizations.md as
  "vanishingly rare", but polymorphic OOP makes it ordinary. One inherited
  (two-level) method call through a `self:m()` site that serves several
  classes permanently disables the IC for every class at that site. That
  includes the one-level ones, which then also pick up the F1 behaviour.
- **L3.** A metatable without `__index` (one set only for `__call`, `__len`
  or `__newindex`) never populates the method IC. `get_index_table_field_direct`
  returns `None` before `set_method`, so every missing-field read on such
  objects takes the full slow route every time.
- **L4.** Reads of absent globals are never cached. `GlobalEnv::read`
  populates only on a hit, so `if OPTIONAL_FLAG then` pays
  `Builtin::from_name` plus a hash probe every time.
- **L5.** `OP_TFOR_CALL` records the call site and flushes the cost batch
  before it knows whether the `next` or `ipairs` fast path will answer. The
  fast paths call nothing, so both are wasted on the common `pairs` and
  `ipairs` loops. Moving them into `instr_tfor_call_rust_fn` and the Lua
  iterator path keeps the "record before any call" rule.
- **L6.** Per-call allocations and memmoves on hot call paths:
  - `collect_varargs` allocates a fresh `Vec` per vararg call, where a single
    shared vararg arena with offsets in `CallInfo` would avoid it;
  - `begin_call`'s `self.stack.remove(idx)` memmoves the arguments for every
    Rust callee;
  - `invoke_rust_fn`'s `rotate_right` moves the callback's whole frame when it
    returns fewer values than it left.
- **L7.** `index_string`'s cache-hit path does `check_stack_space(1)` and a
  push, which the register path then pops. notes/optimizations.md already
  tracks the "string method calls always take the stack slow path" item, so
  this only notes that it is the same F2 scratch dependency.

## Checked and found sound

So the next pass does not redo them:

- The cost batch matches unit-at-a-time charging at the budget edge, given
  that every native charge happens in a Rust callback and every route to one
  flushes first. I traced `OP_CALL`, `OP_TFOR_CALL`, the `__index`,
  `__newindex` and `__len` function handlers, and the string `__index`
  handler.
- The error-path `consume(pending)` cannot be refused except under
  `BudgetExceeded`.
- `return_to_caller` and the `OP_CALL` / `SET_LIST` `resize(frame.top)` stay
  within the caller's earlier reservation.
- `check_window` covers every truncating window.
- `collect_returns` closes upvalues before moving anything.
- `unwind_invocation` and `pop_activation` release chain charges and varargs
  exactly once.
- `reject_admission` undoes admission completely, apart from the location
  issue in F3.
- Weak cache processing covers every runtime a frame can hold: records pin
  their bytecode tree, and closures keep the mapped runtime.
- String-method and field IC validation cannot return a wrong value. Version
  matches read by index and treat nil as a miss.
- The TFOR cursor re-validates against the control key, so recursive loops
  over one table are safe.
