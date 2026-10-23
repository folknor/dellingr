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

## RT-003 - A Lua-to-Lua admission StackOverflow is located inside a function that never ran

Reported independently by the `dispatch` hunter (its F3) and the `vm-core`
hunter (its finding 2). Both rate it low; it misleads the script author.

**Claims broken.** docs/limits.md says such a call "fails there with
StackOverflow, before its body runs", which locates the failure at the call.
The rendered location contradicts it. This is the misleading-message class
AGENTS.md counts as a defect.

**Mechanism.** `admit_lua_call` refuses two ways, and the two report different
locations:

- **`CallDepthExceeded`** is checked before the activation record is pushed.
  The error reaches the caller's unwind and is located at the caller's
  `OP_CALL` line.
- **`StackOverflow`** from the frame reservation (`check_stack_space(grow)`)
  comes after the record is pushed. `reject_admission` builds the callee's
  frame with `ip = 0` (`admitted_frame()`) and calls `report_frame_error` on it,
  so the error gets the callee's `line_info[0]` as its line. The traceback's
  innermost entry, which `Display` uses for the `chunk:line:` prefix, is the
  callee at its first instruction. The attached source name (`host_error` reads
  `call_stack.last()`) is also the callee's chunk. `unwind_invocation` then
  keeps that location and traceback, because the trace is already non-empty.

The author sees `callee.lua:<first line of callee>: stack overflow` for a
function whose body never ran, with a traceback line for it. Two refusals at the
same call site, one per limit, point at different places.

**Direction.** Return admission errors unreported and unlocated, as
`CallDepthExceeded` does, and let the caller's unwind locate them at the call
instruction (or pop the record before reporting, or build the trace from the
caller frame). With nothing left to report, `reject_admission` reduces to the
bookkeeping undo. The `dispatch` hunter checked that `reject_admission`
otherwise undoes admission completely.

## RT-004 - `reg_vararg` truncates the live frame before the stack preflight that can refuse

From the `codegen` hunter, raised as a lateral observation outside its scope.

In `vm/registers.rs::reg_vararg`, the `RetCount::All` arm runs
`check_window(dst)?`, then `self.stack.truncate(dst)`, then
`check_stack_space(available)?`. When the preflight refuses, the live frame's
registers above `dst` are already gone. Today that is harmless:
`unwind_invocation` owns the failed frame, and errors kill the callback. It is
still the one stack-growth site in this area that changes the stack before the
check that can refuse it. The project's preflight rule ("a rejected operation
must leave the stack ... unchanged") and the fault-injection sweep's "surviving
operands" check both expect a clean refusal.

**Direction:** preflight `available - (frame.top - dst)` first, or preflight
before the truncate.

## RT-005 - Numeric `for` control errors claim arithmetic

From the `dispatch` hunter. Low.

`instr_for_prep` and `instr_for_loop` report a non-number start, limit or step
as `TypeError::Arithmetic`, which renders as "attempt to perform arithmetic on a
nil value". `for i = 1, n do` with `n` nil performs no arithmetic the author
wrote, so the message names the wrong operation. That is a misleading message by
AGENTS.md's own standard, not a request to match reference wording.

Separately, `instr_for_loop` re-validates the three hidden control registers on
every iteration. The script cannot write those registers, so the check is dead
work on the hottest loop opcode.

**Direction.** Add a `TypeError` variant naming the `for` field (initial value,
limit, step). Drop the per-iteration re-checks, or make them `debug_assert`s.

## RT-006 - The method IC's `globals_version` guard rests on a rationale that does not hold

From the `dispatch` hunter. Low: the comment is wrong, and the guard costs
performance.

`get_cached_index_table_field` rejects an entry when `globals_version` moved.
The comment says that otherwise "a pre-warmed callsite resurrects the pre-swap
binding inside the sandbox" when `mt.__index = string`.

The cached route already re-validates everything that uncached resolution
reads: the receiver's metatable identity; the `__index` slot's key, with its
value equal to the cached handler; and the method table's version, or the key at
the cached index. None of that reads globals. `mt.__index = string` stores the
table value, so after a `with_restricted_env` swap, uncached resolution also
goes through that same table. A cached hit cannot return anything a cold lookup
would not.

The guard only invalidates. `write_builtin_slot` bumps the version on every
builtin write, so a script assigning to any builtin name (`print = myprint` in a
loop, say) dumps every method IC and every global cache entry. Global caches
never cache builtins (`GlobalEnv::read` returns before populating), so they do
not need that bump either.

**Direction.** Remove the field and the bump on builtin writes. If there is a
hazard the hunter missed, the comment needs to name it.

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

## RT-008 - `open_libs` inside `with_restricted_env` leaves global state the restore does not undo

From the `vm-core` hunter.

**Claim broken.** `with_restricted_env` documents that "The original environment
is restored after the function completes (or errors)".

**Mechanism.** If the host calls `state.open_libs()` inside `f`, it:

- installs fresh library tables into the restricted `GlobalEnv`, which is
  discarded on restore;
- replaces `string_metatable` (and resets `string_mt_index_shape`) State-wide.
  That is not part of the env, so it is not restored, and `s:upper()` afterwards
  resolves through a string library table that no global names;
- with `snapshot`, has `capture_env_tokens` re-key `env_tokens` and
  `env_baselines` to the restricted env's new tables. After the restore the real
  `math`, `string`, `table` and `_G` objects no longer carry tokens, so a save
  classifies them as user tables, and the orphaned canonical objects stay rooted
  for the State's life.

Nothing forbids the combination, and both are public `&mut self` APIs.

**Decided (user, 2026-10-23):** refuse `open_libs` inside a restricted
environment with an error.

**Direction.** Refuse `open_libs` while `transient_roots.suspended_envs` is
non-empty, or document the combination as unsupported.

Related observation from the same hunter, outside the defect bar:
`with_restricted_env` silently scopes `seal_globals()` and `globals_sealed()` to
the restricted env, so a seal inside `f` is lost on restore. That is consistent
with "bindings ... go away with that environment", but seal is documented as
one-way and State-wide.

## RT-009 - Host `get_table` drops the key on a refusal that never enters the handler

From the `tables` hunter.

**Claims broken**

- The doc comment on `get_table_with_key_tracking_call` (src/vm/metamethod.rs):
  "records the commit point for host reads: only entering a user function can
  consume the key on failure".
- `State::get_table` docs: "Validation and dispatch preflight failures leave the
  stack unchanged".
- AGENTS.md: `get_table_unguarded` "restores the key only before handler entry".

**Mechanism.** `handle_index_metamethod_inner` (the function and RustFn arms) and
`call_string_index_handler` (src/vm/eval_index.rs) set `*handler_called = true`
before `call_unguarded`. `call_unguarded` can still refuse without entering
anything:

- `native_call_depth >= MAX_NATIVE_CALL_DEPTH` (`NativeCallDepthExceeded`);
- `begin_call` -> `admit_lua_call`: `MAX_CALL_DEPTH`, and the callee frame's
  `check_stack_space(grow)` (`StackOverflow`).

On each of these, `fail_call` clears the three operands, and
`get_table_unguarded` sees `handler_called` and does not push the key back. Two
`StackOverflow` refusals of the same `get_table` call then leave different
stacks:

- With 0-2 free slots, the refusal comes from `check_stack_space(3)`, which runs
  before the flag is set. The key is preserved, which is what
  `tests/host_table_api_atomicity.rs::get_table_preserves_the_key_on_dispatch_preflight_failures`
  asserts.
- With 3 free slots but fewer than the handler's `max_stack`, the refusal comes
  from the callee-frame admission. The key is consumed.

The same split applies to a host `get_table` made from a Rust callback that is
already at the native depth limit.

**Direction.** Set the commit flag where the callee is actually entered, not
before the call. One way is for `call_unguarded` to report whether it got past
admission, for example a separate admission phase whose refusals carry an
"operands untouched" outcome. The cleaner structural move is a
`try_call_preflight` that runs every refusal check (native depth, Lua depth,
callee stack) before anything is pushed. Then "preflight failure" means one
thing for every host entry point, not just `get_table`.

## RT-010 - An all-dead array window grows instead of being released when the dead-prefix hint lags

From the `tables` hunter.

**Claims broken.** docs/limits.md, memory section: "an all-dead window is
released at the next append or array write outside it, so a FIFO ... keeps
storage proportional to its backlog". The doc comment on `write_outside_window`
says the same: "An all-dead window restarts at `key`".

**Mechanism.** "All dead" is tested as `self.lead == self.array.len()`, but
`lead` is only a lower bound. It advances by at most `LEAD_CATCH_UP` (8) per nil
write at the hint, and not at all for nil writes above it. Clearing a window
from the top down leaves `lead` at about 9, however long the window is.
`write_outside_window` does no catch-up of its own, so it takes the extend
branch, and the downward pad uses `len - lead` as its "live span" estimate:

```lua
local q = {}
for i = 1, 2000 do q[i] = i end
for i = 1, 1000 do q[i] = nil end      -- hint tracks: lead = 1000
q[2001] = true                          -- append drops the prefix: base = 1000, window keys 1001..2001
for i = 2001, 1001, -1 do q[i] = nil end -- top-down clear: lead ends at 9, window is all nil
q[1] = true                             -- outside the window, so it should restart there;
                                        -- instead splices 1000 nils: window = 2001 slots, 1 live
```

Scaled up, a single SET charge doubles a large all-nil allocation that the doc
says is released. That is host work out of proportion to its charge, and a
direct contradiction of the documented release rule. The append path has the
same blind spot: an all-nil window with a lagging hint is kept and pushed onto.
limits.md's "after catching up within a bounded number of pushes" partly covers
that case, though the bound is O(lag / 15) pushes, not a constant. The
outside-window write has no catch-up at all.

**Direction.** Replace the hint with an exact live count of the window, a
`window_live: usize` maintained on every window write (nil to live, live to
nil). That makes "all dead" O(1) and exact. `lead` can stay as a prefix-skip
accelerator for `next` and `mark_values`, but release decisions should never
depend on it. The downward pad in `write_outside_window` should then use the
live count, not `len - lead`.

## RT-012 - `set_metatable_of` rejects a bad metatable as "attempt to index a X value"

From the `tables` hunter. Lower severity.

`State::set_metatable_of` (table_ops.rs) returns
`TypeError::TableIndex(other.typ())` when the value on top is neither a table nor
nil. It returns the same error when the target is not a table, through
`table_operand_below_top`. For a host passing a number as the metatable, the
message reads "attempt to index a number value", but nothing was indexed.
Scripts never see this, because `setmetatable` type-checks argument 2 first. For
the host it is the misleading-message case AGENTS.md counts as a defect.

**Direction.** Give it its own error kind, for example "metatable must be a
table or nil".

## RT-014 - Every slow-path entry repeats the fast-path lookup

From the `dispatch` hunter (its L1) and the `tables` hunter (its L2). Raised as
performance observations outside the defect bar.

The slow-path bullets go with the register-native rewrite and are deferred with
RT-001 (user, 2026-10-23). The `try_insert_table_direct` bullet stands alone.

- `get_field_slow` -> `instr_get_field` re-runs `get_table_field_direct` and
  `get_index_table_field_direct`, both of which just failed in
  `get_field_direct`.
- `set_field_slow` -> `instr_set_field` re-runs `try_set_field_cached` and
  `try_set_field_direct`.
- Every `__index` function or two-level chain read therefore pays the lookups
  twice, plus the push and pop of a copy of an already-rooted register. This is
  the same layering as RT-001 and RT-002, and the register-native rewrite
  proposed there removes it.
- `try_insert_table_direct`, for a table with a metatable, calls `tbl.get(&key)`
  and then `insert`, which repeats the lookup.

## RT-015 - Inline caches that never populate: metatables without `__index`, and absent globals

From the `dispatch` hunter (its L3 and L4). Raised as performance observations
outside the defect bar.

- A metatable without `__index` (one set only for `__call`, `__len` or
  `__newindex`) never populates the method IC. `get_index_table_field_direct`
  returns `None` before `set_method`, so every missing-field read on such objects
  takes the full slow route every time.
- Reads of absent globals are never cached. `GlobalEnv::read` populates only on a
  hit, so `if OPTIONAL_FLAG then` pays `Builtin::from_name` plus a hash probe
  every time.

## RT-016 - Call-path overheads: TFOR_CALL bookkeeping, vararg allocation, argument memmoves

Raised as performance observations outside the defect bar.

- **`OP_TFOR_CALL` bookkeeping** (dispatch hunter, L5). It records the call site
  and flushes the cost batch before it knows whether the `next` or `ipairs` fast
  path will answer. The fast paths call nothing, so both are wasted on common
  `pairs` and `ipairs` loops. Moving them into `instr_tfor_call_rust_fn` and the
  Lua iterator path keeps the "record before any call" rule.
- **Per-call allocations and memmoves** (dispatch hunter, L6).
  `collect_varargs` allocates a fresh `Vec` per vararg call, where a single
  shared vararg arena with offsets in `CallInfo` would avoid it. `begin_call`'s
  `self.stack.remove(idx)` memmoves the arguments for every Rust callee.
  `invoke_rust_fn`'s `rotate_right` moves the callback's whole frame when it
  returns fewer values than it left.

## RT-017 - The pattern cache is cleared wholesale at every collection

Reported as a performance observation outside the defect bar by both the
`vm-core` and the `strings` hunter.

`collect_garbage` clears the whole pattern cache. That is correct, because keys
are string pointers and freed slots can be reused. But the default GC policy
collects at `max(2 * live, 20)` allocations, so in allocation-heavy or
pattern-heavy scripts the "compile once" cache is mostly cold and patterns
recompile constantly. The `strings` hunter adds that `gmatch_iter`'s per-call
`push_string` interning (see LIB-010 in notes/bugs-stdlib.md) can itself run
`maybe_collect` and so clear the cache.

**Direction.** Keep entries whose key string survived the sweep. Either prune
only dead keys after the sweep, as `format_pointer_ids` is pruned with
`val_is_live` (the same safety argument holds, since keys are checked before any
allocation can reuse a slot, and eviction stays deterministic), or key the cache
by content hash plus bytes.

## RT-018 - Collector overhead and silent stale roots

From the `vm-core` hunter, raised outside the defect bar.

- **Performance.** `sweep_bytecode_caches` builds a `Vec<Arc<Bytecode>>` of every
  reachable closure (refcount traffic per live closure) plus a `BTreeSet` per
  collection. Recording bytecode identities during `mark_children` would remove
  the second heap walk and the `Arc` clones.
- **Smell.** `GcHeap::mark` and `StringPool::mark` silently ignore a vacant key.
  A stale root therefore shows up as a later "use-after-free detected" panic at
  the access site instead of at marking. A `debug_assert!` that roots resolve
  would move detection to where the root set is wrong.
- **Smell.** `State::pattern_compilations` is a `pub` method that exists only
  under `debug_assertions` (`#[doc(hidden)]`), so the public API surface differs
  between profiles.

## RT-019 - `debug_assert_invariants` messages say more than the code checks

From the `tables` hunter (its L3), raised outside the defect bar.

The O(1) branch for big maps uses the message "dead_count must equal the number
of nil-valued hash slots" for a `<=` bound, and the lead check covers only the
last 64 slots of the hinted prefix. Both shortcuts are deliberate, for speed.
The messages and doc comments should say so, so that a failure is not misread.
