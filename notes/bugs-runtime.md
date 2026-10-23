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

## RT-001 - Whether an instruction succeeds depends on inline-cache history and syntax (replay divergence)

From the `dispatch` hunter, rated medium: it breaks determinism, but only at a
limit edge. The sticky-miss mechanism it rests on was also reported by the
`tables` hunter (as a performance issue, its L1) and by the `dispatch` hunter's
own lateral L2. Those accounts are merged here.

**Claims broken**

- docs/determinism.md: "The same script and inputs produce the same result and
  the same cost on a given build." A save and load in between must not change
  results. That is the replay contract, and the reason caches are kept out of
  snapshots.
- The `RuntimeCaches` doc in caches.rs presents caches as an unobservable
  acceleration: rebuilt cold on load, cleared on collection, with "a hit trusts
  a matching handle". Being cold is observable here.
- docs/limits.md `MAX_METAMETHOD_DEPTH`: "active steps in an index, newindex or
  non-function call chain". Whether a one-step `__index` lookup counts depends
  on whether the site was written `.k` or `["k"]`, and on cache history.

**Two routes for one lookup.** For `obj.k` or `obj:k()` on a table whose raw
lookup misses and whose metatable `__index` is a table, `reg_get_field` has two
routes:

- **Direct route** (`get_field_direct` -> `get_index_table_field_direct`). It
  answers from the `__index` table with no temporaries, no
  `with_metamethod_depth` and no stack check.
- **Slow route** (`get_field_slow` -> `instr_get_field` -> `get_table_with_key`
  -> `handle_index_metamethod`). It needs one slot above the frame top
  (`check_stack_space(1)` in `get_field_slow`) and enters
  `with_metamethod_depth`, which refuses with `MetamethodDepthExceeded` when
  `metamethod_depth >= MAX_METAMETHOD_DEPTH`.

**Cache state picks the route.** `get_index_table_field_direct` begins with:

```rust
if cache
    .and_then(FieldLookupCacheSlot::get_method)
    .is_some_and(|entry| entry.method_index.is_none())
{
    return None;
}
```

This check runs before any validation, and `set_method` is only called further
down the same function, so the entry is never replaced. The miss is not
revalidated against the method table's version, and it is not keyed on the
receiver's metatable. A miss entry is therefore permanent for the site. It is
written whenever the site once saw any of these:

- a receiver whose `__index` is a function;
- a receiver whose `__index` is a non-table;
- a receiver whose `__index` table lacked the key. That is ordinary two-level
  inheritance: `Derived.__index = Derived`,
  `setmetatable(Derived, {__index = Base})`, then a `Base` method called on a
  `Derived` instance.

From then on every receiver at that site takes the slow route, including
receivers whose method is one level up. With a cold cache (a fresh closure
runtime, or any State after `load_state`, because caches "are never serialized
and are rebuilt cold when a snapshot is loaded"), the same receiver takes the
direct route. The `Some(None)` "known miss" arms in
`get_cached_index_table_field` cannot be reached from its only caller.

**Divergence at the limit edges.** The same instruction on the same receiver
succeeds or fails depending on which receivers the site saw earlier:

- **Metamethod depth at the limit.** A script reaches it without native
  recursion. Lua-to-Lua recursion through a non-function `__call` chain holds
  one chain unit per level: `obj()` where `obj`'s `__call` is a table whose
  `__call` is a Lua function. `begin_callable_call` charges the unit and hands
  it to the admitted record, so 200 levels (`MAX_METAMETHOD_DEPTH`, well under
  `MAX_CALL_DEPTH` = 1000) sit at the limit with native depth 1. There,
  `self.name` on a one-level class instance works with a cold cache and raises
  `MetamethodDepthExceeded` with a warm miss cache.
- **Stack cap.** Admission reserves whole frames. `max_stack` is a u16, and call
  arguments nested in call arguments push it into the hundreds or thousands, so
  `MAX_CALL_DEPTH` frames can reach `MAX_STACK_SIZE` from script alone (a host
  can also push there). With the frame top at the cap, the same `obj.k` succeeds
  through the direct route and raises `StackOverflow` through the slow route.

A continuous run and a run resumed from a snapshot of the same logical state
therefore diverge: one kills the callback, the other continues.

**Syntax picks the route too, with no caches involved.** `reg_get_table` has no
direct `__index` route; its fast path covers only a raw hit or a table without
a metatable. At the depth limit, `obj.k` succeeds and `obj["k"]` raises
`MetamethodDepthExceeded`. String receivers are a third variant:
`index_string`'s first step into the string library table takes no depth unit
at all, cached or not.

**Performance side of the same mechanism.** Results stay correct away from the
limits, but the cost is real. notes/optimizations.md tracks the sticky miss as
"vanishingly rare"; the `dispatch` hunter argues that polymorphic OOP makes it
ordinary. One inherited (two-level) method call through a `self:m()` site that
serves several classes disables the IC for every class at that site, including
the one-level ones. The `tables` hunter's repro:

```lua
local Base = {}
local o = setmetatable({}, {__index = Base})
local function get(x) return x.greet end
get(o)              -- miss is cached as "method_index = None"
Base.greet = print
for i = 1, 1e6 do get(o) end   -- always the slow path, for every receiver
```

**Deferred (user, 2026-10-23),** together with RT-002 and the register-native
slow-path rewrite they share. When reopened, the user's ruling on the depth
question is: a non-nesting `__index` table step never counts against the depth
limit; only steps that recurse take a unit, on every route.

**Direction**

- Decide what one non-nesting `__index` table step costs against the depth
  limit, and make every route implement that one rule. Either never count it
  (only steps that recurse, such as a second table level or a handler call,
  take a unit), or always count it (the direct route then checks the limit
  too). The first matches what the direct route does today and needs no stack.
- Stop the sticky miss from deciding the route. Either re-resolve on a miss
  entry, revalidate negative entries against the version (the check is already
  there) and let them repopulate, drop the early return and let
  `get_cached_index_table_field`'s validation handle it, or stop caching misses.
- Structurally: the slow paths exist only to reuse the stack-protocol helpers
  (`instr_get_field`, `instr_set_field`, `instr_get_table`, `instr_length`).
  Those helpers repeat the fast-path lookups and need scratch for operands that
  are already rooted in registers. Rewriting them register-native, with
  operands by value, results written to `dst`, and stack use only for an actual
  handler call window, removes this whole class of route-dependent behaviour.
  It also removes the double lookup in RT-014. RT-002 covers the slow route's
  stack scratch.

## RT-002 - "Its bytecode never checks it again" is false: register ops still fail StackOverflow mid-body

From the `dispatch` hunter, rated low to medium: a contract statement is wrong,
and the behaviour is inconsistent.

**Claims broken.** `admit_lua_call` says:

> The frame reserves every register it can use, parameters, locals and
> temporaries, against the stack cap now, so its bytecode never checks it
> again.

docs/limits.md frames the cap the same way: a call "fails there with
`StackOverflow`, before its body runs".

**Mechanism.** Several plain register instructions still grow the stack above
the frame top and check the cap. None of them is a metamethod path:

- `OP_CLOSURE`: `reg_closure` -> `instr_closure` -> `push_closure`, which does
  `check_stack_space(1)`, then pushes and pops into `dst`.
- `OP_NEW_TABLE_TEMPLATE`: `new_table_with_template` does
  `check_stack_space(1)`. `OP_NEW_TABLE` and `OP_NEW_TABLE_PRESIZED` allocate
  straight into the register and never check. So at the cap `{}` works and
  `{x = 1}` raises `StackOverflow`.
- `OP_CONCAT`: `reg_concat` does `check_stack_space(count)` and copies every
  operand above the frame, then concatenates there. So `a .. b .. c` needs 3
  free slots above a frame that already owns `a`, `b` and `c`.
- Every slow path needs scratch: `get_field_slow` 1, `get_table_slow` 2,
  `set_field_slow` 2, `set_table_slow` 3, `len_slow` 1, plus 3 or 4 more for a
  handler call. Through RT-001 this need depends on cache state.
- `index_string`'s cache-hit path does `check_stack_space(1)` and a push, which
  the register path then pops. notes/optimizations.md already tracks "string
  method calls always take the stack slow path"; this is the same scratch
  dependency.

A frame whose top sits within a few slots of `MAX_STACK_SIZE` is admitted, then
dies partway through its body on a closure, a template constructor or a
concatenation. Where it dies depends on which of these it reaches first.

**Side note on `OP_CLOSURE`.** `instr_closure` calls `find_or_create_upvalue`,
which inserts into `open_upvalues` and allocates pool slots, before
`push_closure`'s preflight refuses. The rejected instruction is therefore not
side-effect free. It is harmless only because the error always kills the frame,
and unwinding closes those upvalues.

**Deferred (user, 2026-10-23),** together with RT-001.

**Direction.** Make the claim true rather than reword it:

- allocate the closure and the template table directly into `dst`, as
  `alloc_table` already does;
- concatenate from the register range in place (`concat_helper` only needs a
  slice);
- reserve a fixed handler headroom at admission, the largest call window a slow
  path opens (4), so that only open-results windows and Rust callbacks can push
  past a frame's reservation.

Admission then fails up front, as limits.md describes. If the headroom is not
wanted, fix the comment and limits.md to list the instructions that still check.

## RT-007 - `on_error` fires for a swallowed nested error only if it came from a Lua frame

From the `vm-core` hunter.

**Claim broken.** `HostCallbacks::on_error` (src/host.rs) is "Fired exactly once
per error, in addition to that error being returned from `call()`". It lists
"direct calls of non-callable values", `__call` chains and "result padding" as
covered.

**Mechanism.** How reporting is decided:

- An error that surfaces inside a Lua frame is reported unconditionally by
  `unwind_invocation` -> `report_frame_error` -> `host_error`.
- An error raised at a call boundary goes through `fail_call` ->
  `report_call_error`. That reports only when `is_outermost_host_boundary()`
  holds (`call_depth == 0 && active_rust_fn.is_none()`). Otherwise reporting is
  left to "the enclosing activation or callback", which reports only if the
  error propagates back to it.

So when a Rust callback calls `state.call(...)` and handles the `Err` instead of
returning it, the outcome depends on where the nested error came from:

- If the callee is a Lua function that raises (`error()`, a type error, the
  budget), `on_error` fires once.
- If the callee is a non-callable value, a `__call` chain that hits
  `MetamethodDepthExceeded` or `TooManyArguments`, or a Rust function that
  returns `Err`, or if a fixed result count's nil padding overflows the stack,
  or the call is refused with `NativeCallDepthExceeded`, then `on_error` never
  fires, although `call()` returned the error.

`tests/host_error_reporting.rs` covers only callbacks that propagate the nested
error (`rust_callback_propagating_nested_*`), where both kinds come out at
exactly one. There is no swallowing case.

**Deferred (user, 2026-10-23).**

**Direction.** Decide which rule is the contract:

- "Report at the first boundary that returns it to code that may continue": then
  `fail_call` reports whenever it returns into a callback, not only at the
  outermost boundary. `mark_reported` already makes later boundaries idempotent.
- "Report only what reaches the host": then the Lua-frame unwind must stop
  reporting unconditionally.

Then make the `on_error` doc say which one it is.

## RT-014 - Every slow-path entry repeats the fast-path lookup

From the `dispatch` hunter (its L1) and the `tables` hunter (its L2). Raised as
performance observations outside the defect bar.

Deferred with RT-001 (user, 2026-10-23): these go with the register-native
rewrite.

- `get_field_slow` -> `instr_get_field` re-runs `get_table_field_direct` and
  `get_index_table_field_direct`, both of which just failed in
  `get_field_direct`.
- `set_field_slow` -> `instr_set_field` re-runs `try_set_field_cached` and
  `try_set_field_direct`.
- Every `__index` function or two-level chain read therefore pays the lookups
  twice, plus the push and pop of a copy of an already-rooted register. This is
  the same layering as RT-001 and RT-002, and the register-native rewrite
  proposed there removes it.

## RT-016 - Call-path overheads: TFOR_CALL bookkeeping, vararg allocation, argument memmoves

Raised as performance observations outside the defect bar. The fifth
resolution wave replaced `invoke_rust_fn`'s whole-frame `rotate_right` with a
copy of the retained results; the rest was deferred with comments in
src/vm/eval.rs and src/vm/frame.rs, pending native-frame measurement:

- **`OP_TFOR_CALL` bookkeeping** (dispatch hunter, L5). It records the call site
  and flushes the cost batch before it knows whether the `next` or `ipairs` fast
  path will answer. The fast paths call nothing, so both are wasted on common
  `pairs` and `ipairs` loops. Moving them into `instr_tfor_call_rust_fn` and the
  Lua iterator path keeps the "record before any call" rule. It touches
  `Frame::run`'s live ranges, so measure `native_bytes_per_level` with it.
- **Per-call allocations and memmoves** (dispatch hunter, L6).
  `collect_varargs` allocates a fresh `Vec` per vararg call, where a single
  shared vararg arena with offsets in `CallInfo` would avoid it; that needs
  coordinated State, `CallInfo`, GC-root and snapshot-quiescence changes.
  `begin_call`'s `self.stack.remove(idx)` memmoves the arguments for every Rust
  callee.

## RT-022 - Generic `call_unguarded` and the admission-reporting move need their on-demand checks

Lateral from the second resolution wave. To mark handler entry only after
admission succeeds, `call_unguarded` became generic over `impl CallArguments`
and `begin_call` takes an `impl CallEntry`, so two monomorphized copies of
functions that stay live across native re-entry now exist. The standing
native-stack tests check that the envelope still fits; nobody has compared the
per-level bytes. Run `brokkr test native_bytes_per_level` (dev and `--release`,
default and `snapshot`) against the parent commit, and attribute any growth
with `scripts/frame_sizes.py`.

The same wave stopped `reject_admission` from reporting the unentered callee;
host calls now report through `fail_call`. The on-demand
`error_boundary_fault_sweep` (`--features fault-injection,snapshot`) checks
notification counts per event and has not been run since.

The third wave added more that the standing gate does not exercise:
`gmatch_refused_push_preserves_the_next_match` (tests/string_bytes.rs) is gated
on `fault-injection`, and `brokkr check --features hotpath` has not been run
since annotations moved (`get_table_unguarded` lost its `hotpath::measure`).
Fallible table growth also reshaped `append`, `insert` and
`try_insert_table_direct` (inlined into `Frame::run`), so the
`native_bytes_per_level` comparison covers that too.
