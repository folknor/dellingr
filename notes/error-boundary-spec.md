# Implementation spec: boundary-owned error cleanup

Status: design proposed by spar (codex, read-only profile, 2026-10-22) and
revised over five review rounds the same day. The fourth round (two reviews,
validated against 6a49df8; neither review file was committed, and the
disposition at the end summarizes each finding inline so nothing depends on
them) moved the `__call` chain charges to a local owner, deleted
`call_chain_depth`, removed `call_from_lua`'s error arm and the debug mark
stack, and made the fault injector an on-demand tool rather than a check
shape. The fifth round (one review, also summarized in the disposition) kept
`invoke_rust_fn`'s `stack_bottom` restore on error, fixed the injector's
countdown mechanics, moved the at-rest check out of line, trimmed the
transcript A/B, made the stop conditions explicit, and added
`tostring_bytes` to the boundary inventory. Not started.
Source item: HYG-045, "Error-path cleanup decided per site", in
`notes/deferred.md`. No bz tickets exist for it (`notes/hyg-045-tickets.md`).

**Signed off.** The HYG-045 entry ends with "Decision (2026-10-22): deferred;
stays here." Later the same day the user named HYG-045 as the goal of this
spec loop and approved its roster and modifiers; that commission is the
un-deferral, so the implementer proceeds without asking again. This spec
argues the deferral's gate was the wrong axis (see Reinterpretation). The
un-deferral must be visible where the deferral is: the first commit of this
work appends to the HYG-045 entry a line recording it (date, and that this
spec carries the work). The entry itself is removed once the work lands
(last bullet under Changes).

## Problem

After an `Err`, three things must be put back: the value stack above the
failed operation, `State::stack_bottom`, and the `__call` chain charges (units
of `metamethod_depth` and `call_chain_depth` that `begin_callable_call` adds
through a `&mut u32` parameter owned by `call_from_rust` / `call_from_lua`).
Today each exit does it by hand:

- `src/vm/eval.rs`: `call_unguarded` (the native-depth refusal calls
  `truncate_call_frame`); ten `Err` exits wrap their error in `fail_call`
  (one in `call_from_rust`, one in `finish_call`, two in `begin_call`, six in
  `begin_callable_call`); `invoke_rust_fn` has two separate
  `truncate_call_frame` sites.
- `src/vm/metamethod.rs`: `handle_index_metamethod_inner` and
  `handle_newindex_metamethod_inner` truncate their handler-table temporary on
  error.
- `src/vm/eval_index.rs`: `index_string` truncates its handler-table
  temporary on error.
- `src/vm/eval_control.rs`: `instr_tfor_call_rust_fn` truncates on a
  `balance_stack` error.

(The deferred entry's list omits the `__newindex` and generic-for sites.)
Adding a fallible step to any of these bodies means remembering another copy
of the cleanup.

## Reinterpretation of the deferred entry

The entry framed the fix as a choice between a Drop guard (impossible: it
would hold `&mut State` while the body uses it), a closure helper (rejected for
frame size on `call_unguarded`, which stays live across every native re-entry)
and a hand-disarmed guard (keeps the per-site count), and gated it on a
measurement that a closure is frame-neutral.

That is the wrong axis. This spec uses neither a guard nor a closure. Cleanup
moves to the error **boundaries** and to the **owner** of each resource: a
boundary records what it needs on entry and restores it in exactly one error
arm; an owner releases what it took in one arm of its own; bodies beneath
propagate with `?`. Each is ordinary straight-line code (entry, body, one
`match`), so it adds no closure frame. It is the shape of reference Lua's
`luaD_pcall`, which saves `top`, `ci` and `nCcalls` on entry and restores them
in one place. The measurement the entry asked for stays, as supporting
evidence that no native-stack cycle grows (see Acceptance), not as a gate on
whether to start.

Panics are out of scope: they poison the State
(`reference/error-reporting.md`, "Panics and poisoning"), so only `Err`
returns need cleanup.

## The rule

> Ordinary bodies propagate errors. Each resource owner releases what it
> took before propagating, and its correctness depends on no other owner.
> A call made from Rust has one boundary, `call_unguarded`, which truncates
> its operand suffix and restores its stack bottom; inside a Lua invocation
> the dispatch-loop unwind does that. Any entry point that can return an
> error to code which then continues must be a boundary.

What is structural: adding a fallible step under a boundary needs no new
cleanup. What it is not: a general transaction. State mutated by a body
outside the resources below still needs its own owner, and truncation cannot
repair a body that destroyed slots below the boundary's callee slot (the
frame-isolation invariant still carries that).

The cost of the rule, stated so it is not paid by accident: today the inner
truncations protect *any* caller of the index and newindex paths. After the
change only `call_unguarded`, `get_table_unguarded` and the dispatch-loop
unwind do. The guarded host entry points are `call`, `call_anchor`,
`get_table`, `tostring_bytes` and `with_restricted_env`; `call_anchor` and
`tostring_bytes` need no boundary of their own because every temporary they
push (the inserted callee; the `__tostring` handler and its receiver) sits at
or above the `idx` of the `call_unguarded` they then enter, so that call's arm
clears them. A future host-facing API that dispatches `__index` / `__newindex`
(or calls) without being a boundary would leak handler-table temporaries
silently. The debug at-rest check at `State::guarded` (section 6) catches the
bookkeeping half of such an omission once the outermost host call returns
(counters, records, roots, `stack_bottom`); a leaked operand slot is caught
only by API-specific tests of that entry point's stack contract.

Ownership after the change:

| State | Owner |
|---|---|
| Call operand suffix (callee slot upward) | `call_unguarded`: `truncate(idx)`; inside a Lua invocation, `unwind_invocation` |
| `stack_bottom` | Whoever moves it moves it back: `invoke_rust_fn` on both paths, admission (`reject_admission`), `return_to_caller` and `unwind_invocation`. `call_unguarded`'s arm writes the entry value as a backstop and debug-asserts it was already there |
| `__call` chain units during resolution and a Rust callee | `begin_callable_call`, in a local, one release arm |
| `__call` chain units of an admitted Lua callee | its `CallInfo`: released by `return_to_caller`, `run_invocation`'s success path and `unwind_invocation` (each from `pop_activation`) |
| `call_stack`, `call_depth`, `vararg_stack` | admission rollback (`reject_admission`), activation return and `unwind_invocation` |
| `pending_bases` | each Lua frame's return and unwind, from its own floor |
| Open upvalues | frame return and unwind, closed before slots go |
| `transient_roots` values | `with_rooted_values` and other root owners |
| Suspended environments | `with_restricted_env` |
| `active_rust_fn`, method-call argument numbering | `invoke_rust_fn` |
| `native_call_depth` | `call_unguarded`'s bracket |
| index/newindex `metamethod_depth` units | `with_metamethod_depth` (unchanged) |
| Table-operation temporaries | `get_table_unguarded` for the public `get_table`; otherwise the enclosing `call_unguarded` or dispatch-loop unwind |
| `call_anchor`'s inserted callee, `tostring_bytes`'s handler and receiver | The `call_unguarded` they enter (they sit at or above its `idx`) |

## Changes

### 1. Chain charges and call boundaries in `src/vm/eval.rs`

**Chain units get one owner per phase.**

- *Resolution.* `begin_callable_call` holds the units it acquires in a plain
  local. Every error exit, before or after an acquisition, leaves through one
  release arm inside `begin_callable_call` that subtracts the local from the
  counter and propagates. Write the loop body as a labeled-break loop or an
  `#[inline(always)]` inner body, not as a separate out-of-line function:
  `begin_callable_call` is live across native re-entry (it calls
  `invoke_rust_fn`), and a second non-inlined frame there would be too.
- *Rust callee.* `invoke_rust_fn` runs inside `begin_callable_call` with the
  units held, so they still bound nested chains and index/newindex chains
  during the callback, as today. On `Ok` `begin_callable_call` releases them
  before returning `Some(n)`; on `Err` its release arm does.
- *Lua callee.* After `admit_lua_call` succeeds, `begin_callable_call` writes
  the local into the new record's `chain_charges` (`call_stack.last_mut()`),
  for calls from Lua and from Rust alike. Admission failure happens before
  that transfer, so the record `reject_admission` pops holds zero and the
  release arm releases the local. From the transfer on, the activation owns
  the units: `return_to_caller` releases what `pop_activation` returns before
  calling `finish_call`; `run_invocation`'s success path releases what
  `pop_activation` returns (replacing `debug_assert_eq!(charges, 0)` and the
  comment that an entry activation's charges stay with its Rust caller); and
  `unwind_invocation` releases as today.

Consequences: no boundary holds chain units when an `Err` reaches it or when
`finish_call` runs; `begin_call`'s two function fast paths never touch the
counters (today every call from Lua to a Rust function runs
`release_chain_charges(0)` in `finish_call`); the `charges: &mut u32`
parameter through `call_from_rust`, `begin_call`, `begin_callable_call`,
`fail_call` and `finish_call` goes, and with it the `mem::take` /
double-release hazard. Each owner's correctness is local: none relies on
another owner having restored its units, which is what a watermark on a
shared counter would have required.

Units are therefore released **before** result adjustment for every callee
kind (a Rust callee's and an entry activation's included), not after.
`balance_stack` runs no callback and no GC, so this is unobservable, and
today's `fail_call` already releases before reporting.

**`call_chain_depth` is deleted.** Its only reader is
`is_outermost_host_boundary`, where it suppresses the report that
`invoke_rust_fn` makes for a Rust callback failing inside a chain, before
the boundary has released the chain. After section 2 `invoke_rust_fn` no
longer reports, and the only remaining `report_call_error` call is
`call_unguarded`'s error arm, reached after that call's own units were
released (by `begin_callable_call` or by the activation's unwind). Any units
still held at that point belong to an outer owner: an enclosing
`begin_callable_call` running a Rust callee (`active_rust_fn` is `Some`) or an
activation (`call_depth > 0`). So `is_outermost_host_boundary` becomes
`call_depth == 0 && active_rust_fn.is_none()` with identical results.
`metamethod_depth` alone carries the units, counting both kinds against
`MAX_METAMETHOD_DEPTH` as today. The reason the field was kept apart from
`metamethod_depth` (a Rust `__index` / `__newindex` handler failing under a
host table access must still report at its own call boundary, where
`metamethod_depth` is 1) holds for the new predicate too, since it does not
read `metamethod_depth`. This removes the field, its snapshot quiescence
scalar (not serialized, so not replay-visible; `validate_quiescent` accepts
and rejects the same States) and its use in
`src/vm/save_state/integrity_tests.rs`. Guards in
`tests/host_error_reporting.rs`:
`host_table_read_through_failing_rust_index_notifies_once`,
`host_call_through_callable_table_chain_notifies_once`,
`host_call_of_cyclic_call_metamethod_notifies_once`,
`host_call_through_direct_rust_call_metamethod_notifies_once`,
`host_call_through_chain_into_failing_lua_function_notifies_once` and
`callable_argument_overflow_notifies_once`.

**`call_unguarded`** becomes the single boundary for every call made from
Rust. Entry records `idx = call_base(num_args)?` and `old_stack_bottom`. The
body is the native-depth check, the bracket increment,
`begin_call` / `run_invocation` / `finish_call`, and the bracket decrement.
Exactly one error arm follows the bracket:
`Err(self.fail_call(e, idx, old_stack_bottom))`.

- `call_from_rust` has one caller. Fold its body into `call_unguarded` (or
  make it `#[inline(always)]`), so `idx` and `old_stack_bottom` are not live
  in two frames across the re-entry. If it is folded, update the AGENTS.md
  hotpath paragraph, which lists `call_from_rust` among the functions live
  across native re-entry.
- The depth refusal stops truncating on its own and joins that arm. This adds
  a `report_call_error` call there, which is a no-op: reaching another
  `call_unguarded` while the native limit is occupied requires running inside
  an active Lua activation or Rust callback, so `call_depth > 0` or
  `active_rust_fn` is `Some` and `is_outermost_host_boundary` is false.
- `fail_call` moving outside the native-depth bracket changes nothing:
  `native_call_depth` is not part of `is_outermost_host_boundary`, and
  `on_error` receives only `&Error`.
- By the time the arm runs, `stack_bottom` is already the entry value on
  every path: resolution errors never moved it, `invoke_rust_fn` restores it
  on `Err` (section 2), `reject_admission` and `unwind_invocation` restore
  it, and a `finish_call` failure comes after a successful return. The arm
  still writes it (one store in cold code), as the boundary's backstop, and
  section 6 debug-asserts it was already equal.
- The `DynamicArgumentsUnsupported` and `call_base` preflight errors stay
  before the boundary is entered, unreported and with nothing to clean, as
  today (host misuse, excluded from `on_error` by the contract in `host.rs`).

**`call_from_lua`** (dispatch `OP_CALL`) gets **no** error arm. Its only
caller is the `OP_CALL` arm, which propagates with `?` through `Frame::run`
(which only settles pending cost) into `unwind_invocation`, which truncates to
the frame bottom (below `idx`), closes upvalues before the slots go, restores
`stack_bottom` and reports with the caller's frame. `stack_bottom` is already
the frame's when the `Err` leaves `call_from_lua` (every mover restores its
own transition, section 2), and nothing in between reads the stack above the
frame: `report_frame_error` reads the frame and `call_stack`,
`build_stack_trace` reads `call_stack`, and `on_error` gets only `&Error`.
This is the argument section 2 makes for the generic-for path. With chain
units no longer held at the boundary, nothing
else needs releasing, and the admission transfer has moved into
`begin_callable_call`. `call_from_lua` becomes: `idx`, `old_stack_bottom`,
`begin_call(...)?`, then `finish_call(expected, n)?` on `Some(n)` and
`Ok(true)` on `None`. Its doc ("any `__call` chain charges handed to the
record") is rewritten to say `begin_callable_call` hands them over.

**`begin_call` / `begin_callable_call`** lose every `fail_call` wrap and the
`charges` parameter. `begin_call`'s fast paths return `invoke_rust_fn`'s and
`admit_lua_call`'s errors directly; `begin_callable_call`'s exits go through
its release arm.

**`finish_call`** loses `idx`, `old_stack_bottom` and the charge count:
`finish_call(expected, actual)`. It only trims or pads. A padding failure
propagates: from `call_unguarded` into its arm, from `call_from_lua` and
`return_to_caller` into `unwind_invocation`.

`return_to_caller` needs no cleanup of its own beyond releasing the popped
activation's units before `finish_call`: the callee's activation is popped and
`stack_bottom` set to the caller's before `finish_call`, and
`unwind_invocation` truncates to the caller's frame bottom (below
`call_base`), restores `stack_bottom`, and reports with the caller's frame.
`report_call_error` is a no-op inside a Lua invocation.

**`fail_call`** becomes `fail_call(error, idx, old_stack_bottom)`: truncate,
restore `stack_bottom`, report. It stays `#[cold] #[inline(never)]`, and holds
the debug checks of section 6. The order "release before report" no longer
matters for reporting, since the predicate reads no chain counter.

### 2. `invoke_rust_fn`

Owns exactly what it changes for itself: `active_rust_fn` (restored before
any outcome is built), `stack_bottom` (it moves it to `call_base` on entry
and moves it back on **every** non-panic return, `Err` included), and the
method-call argument numbering (`settle_method_call`). It does **not**
truncate or report on `Err`; every path out of it already has an owner for
those:

- `begin_call` / `begin_callable_call` return to `call_unguarded`, whose arm
  truncates to the same `idx` and reports; or to `call_from_lua`, which
  propagates into `unwind_invocation` (section 1).
- The generic-for fast path runs in the dispatch loop; `unwind_invocation`
  truncates to the frame bottom. `report_call_error` there is a no-op today
  (`call_depth > 0`).

Keeping the `stack_bottom` restore is the rule applied to the function that
moved it, the same way admission keeps its own rollback: one store in the
error branch, and no path between an `Err` and its boundary ever runs with
the callee's `stack_bottom` in place, so nothing added on those paths later
(a relative-index read, a `get_top`) can be silently wrong. The earlier
draft dropped the restore and relied on a proof that nothing in between
reads `stack_bottom`; that proof was true but was an invariant a later edit
would break invisibly.

Reporting therefore moves from `invoke_rust_fn` to `call_unguarded`'s arm for
a Rust function the host calls directly. The boundary reports after
truncating instead of before; `on_error` cannot observe the difference (it
sees only the `Error`, and `host_error` reads `call_stack`, which
`invoke_rust_fn` does not touch).

Order on return: poison check, restore `active_rust_fn`, then on callback
`Err` restore `stack_bottom`, settle the numbering and return it; on `Ok`
validate the result count **with the callback's `stack_bottom` still in
place** (`available = self.get_top()` counts the callback's frame; restoring
first would count caller slots and admit an invalid count), on over-report
restore `stack_bottom` and return `ResultCountExceedsStack`, otherwise
compact and restore `stack_bottom`.

Its doc comment ("On error the stack is truncated to `call_base`,
`stack_bottom` and the active Rust function are restored, and the error has
passed through `report_call_error`") becomes "On error `stack_bottom` and the
active Rust function are restored; the callee's slots stay for the enclosing
boundary or unwind to clear, and reporting happens there". The inline
invariant comment ("the caller's active Rust function and stack_bottom are
both restored before a callback error propagates") stays true and stays.

### 3. Generic-for fast path (`src/vm/eval_control.rs`)

`instr_tfor_call_rust_fn` drops its `truncate(call_base)` on a
`balance_stack` error. It runs in the dispatch loop; `unwind_invocation`
truncates the frame.

### 4. Table-access boundary

**`get_table_unguarded`** (`src/vm/table_ops.rs`): on any error, always
`truncate(top - 1)`; re-push the key only when no handler ran. Today it
truncates only when no handler ran and relies on the inner per-level
truncations otherwise; the observable result is identical (stack height
`top - 1` after a handler error, key restored before one). Update its comment.
This is the only table boundary a host can observe, through the public
`State::get_table`. Library callers of `get_table_unguarded` all propagate
with `?`, so for them the arm is invisible.

**`set_table`** (library `__newindex` dispatch, `pub(crate)`) gets **no** arm.
Every caller (`src/lua_std/table.rs`) propagates into `call_unguarded` or the
dispatch-loop unwind, so by the rule it is not a boundary, and an arm there
would be untestable. Its doc comment gains one sentence: on error the key,
the value and any handler temporaries above them are left for the enclosing
call boundary or unwind. A future public `__newindex`-aware setter becomes a
boundary under the AGENTS.md rule (section 7), with a test that can see it.

**`tostring_bytes`** (`src/vm/table_ops.rs`, the third guarded entry that
can run user code) gets **no** arm and no change. `tostring_rendering`
preflights two slots, pushes the handler and the receiver, and enters
`call_unguarded` with the handler as its callee: both temporaries sit at or
above that call's `idx`, so its arm clears them, and after a successful
handler the only remaining temporary (the result) is popped before
`TostringMustReturnString` can be raised. Its doc comment gains the sentence
that a handler failure leaves the stack as it was. The `__tostring` corpus
case (Acceptance 4) pins that height.

Neither promises rollback of writes a handler already made (tables, upvalues,
host data); those survive an error by design.

With that, delete the error-path truncations of the handler-table temporary
in `handle_index_metamethod_inner`, `handle_newindex_metamethod_inner` and
`index_string`. Every caller of `get_table_with_key_tracking_call` (including
through `get_table_with_key`), `set_table_with_key` and `index_string` is
either an `instr_*` function reached only from the dispatch loop, which
propagates into its unwind, or `get_table_unguarded` / `set_table`; none
catches the error and continues. Recursive dispatch passes through
`with_metamethod_depth`, which only restores its own counter.

### 5. Unchanged

- `admit_lua_call` / `reject_admission`: the admission transaction (record,
  `call_depth`, reporting with the admitted frame) keeps its own rollback.
  `reject_admission`'s popped charge count is zero by construction (the
  transfer happens after admission); section 6 asserts it.
- `unwind_invocation`, `Frame::run`'s pending-cost settlement.
- `with_metamethod_depth`: a resource owner of its own unit. It is a closure
  helper already live across `call_unguarded` on the `__index` and
  `__newindex` function paths; that shows such closures are not inherently
  unacceptable, but proves nothing about a new wrapper's frame, and nothing
  here needs it changed.
- `eval_chunk` (test-only) admits and runs an activation directly, outside the
  call-boundary contract; no chain units are involved.

### 6. Debug assertions

No entry-value bookkeeping and no mark stack. Section 1 leaves no state that a
boundary must police across nested owners, and entry values held anywhere on
the native frames of `call_unguarded`, `call_from_lua` or `State::guarded`
would grow dev-profile frames on every native cycle (`tests/native_stack.rs`
pins that envelope). Release builds carry nothing extra.

**Local checks in cold code.** In `fail_call`, which is out of line and not
live across re-entry: `stack.len() >= idx`; `stack_bottom ==
old_stack_bottom` before the backstop write (every mover restored its own
transition, section 2); and no open upvalue at or above `idx` (callee-owned
ones were closed by the unwind; caller-owned ones sit below the operand
suffix). In `reject_admission`: the count `pop_activation` returns is zero.

**At-rest check at `State::guarded`.** On the non-panic, non-poisoned return,
when `active_rust_fn.is_none()` after `f` returns, `debug_assert` that the
in-flight state is at rest: `metamethod_depth`, `call_depth`,
`native_call_depth` and `stack_bottom` are zero; `call_stack`,
`vararg_stack`, `pending_bases`, `open_upvalues`, `pending_bytecode_caches`
and `transient_roots.values` are empty; `active_rust_fn` is `None`.

- The check body lives in a `#[cold] #[inline(never)]
  #[cfg(debug_assertions)]` method (`debug_assert_at_rest`), and `guarded`
  makes one call to it under `cfg(debug_assertions)` after the
  `active_rust_fn` test. `guarded` is generic, monomorphized per entry point
  and on the `host_callthrough` cycle; a dozen inline field reads in a
  `debug_assert!` would otherwise land on its dev-profile frame.
- The gate needs no entry value, so nothing lands on `guarded`'s frame. Rust
  code re-enters a
  guarded entry point only from a `RustFunc` (`HostCallbacks` receive no
  State), every `RustFunc` runs inside `invoke_rust_fn` with `active_rust_fn`
  set, and `invoke_rust_fn` restores the previous value unconditionally on
  every non-panic return, before any branch. So after `f` returns,
  `active_rust_fn` is `None` exactly when the call was entered from host code
  outside any callback.
- Two exclusions. The value stack: host values legitimately remain, and
  results change its height per API. `transient_roots.suspended_envs`:
  `with_restricted_env` is a guarded entry whose closure is host code, so a
  `State::call` inside it passes the gate while the saved environment is
  legitimately held; `with_restricted_env` pops it before its own guarded
  return.
- One source for the list. `validate_quiescent` and its inventory macro live
  in `src/vm/save_state.rs`, which compiles only with `snapshot`, and the
  macro has no per-item `cfg`. Generate the at-rest predicate from the same
  inventory, moved to an ungated place, so the two lists cannot drift (the
  at-rest predicate is the quiescence set minus `stack` and
  `suspended_envs`).
- These are not quiescence checks for nested calls: inside a callback the
  State is legitimately not at rest, and the gate skips it. A leak in a nested
  call surfaces at the outermost return instead of at the leaking site; the
  sweeps (Acceptance 4) pin which case leaked.

What these checks prove, and what they do not: they show inner owners
finished by the time the host regains control; nothing repairs them. Equal
or empty lengths detect leaked entries, not caller-owned entries removed and
replaced by the same number of others. "No open upvalue at or above `idx`"
does not show that caller-owned upvalues below it were preserved. No check
here sees operand contents or stack height, so an entry point that leaks only
a handler-table temporary passes them. The focused tests in Acceptance 4
cover those gaps behaviorally, checking the preserved operands where an API
promises them, not only the height. The checks run only under
`debug_assertions`; they give the sweeps their teeth in the dev profile and
prove nothing about release builds, which Acceptance 4 tests directly.

The fault injector's countdown and event log (Acceptance 4) are
execution-lifetime `State` fields under `fault-injection`, so classify them
for the snapshot in-flight inventory in `src/vm/save_state.rs` (the macro's
comment requires it): either a `cfg`-gated `collections` entry (which needs
the macro change above) or a stated reason they are exempt (test bookkeeping,
disarmed whenever the State is quiescent), backed by a `debug_assert` in
`validate_quiescent`. Neither may enter saved bytes.

### 7. Documentation

- `AGENTS.md`, Architecture: add the rule and the boundary list beside the
  stack-cap invariant paragraph (the boundaries: `call_unguarded`,
  `get_table_unguarded`, `unwind_invocation`; the guarded entries that ride
  on `call_unguarded` without a boundary of their own: `call_anchor`,
  `tostring_bytes`), so the next fallible step added under a boundary is
  written with `?`, a new resource gets one owner that releases it locally,
  and a new host entry point that can return an error to continuing code is
  made a boundary.
- `AGENTS.md` hotpath paragraph, if `call_from_rust` is folded (section 1).
- Doc comments the change makes false, all rewritten in the same commit:
  - `CallInfo::chain_charges` in `src/vm.rs` ("Each holds one unit of
    `metamethod_depth` and `call_chain_depth` until the call completes,
    result adjustment included"): one counter now; set for entry activations
    of calls from Rust too; released when the record is popped, before result
    adjustment.
  - `State::call_chain_depth` goes with the field; `is_outermost_host_boundary`
    gains a comment on why no chain counter is needed (section 1).
  - `finish_call`'s doc ("Adjust ... then release") and its inner comment
    ("Clear the call's frame on failure like every other error path here").
  - `begin_call`'s doc: the units are `begin_callable_call`'s local, released
    on a Rust callee's return and transferred on admission; the paragraph "On
    error the charges are released, the call's slots are cleared, the
    caller's stack_bottom is restored, and the error has passed through
    `report_call_error`" is replaced by who does each.
  - `fail_call`'s doc paragraph on errors passing through "without a Lua
    frame or RustFn reporting boundary", and its note that releasing first
    lets the outermost host call see the chain depth at zero: it is now
    `call_unguarded`'s arm alone.
  - `call_unguarded`'s refusal comment, and `call_from_rust`'s doc (or its
    removal if folded); `call_from_lua`'s doc (section 1).
  - `run_invocation`'s success-path comment and `debug_assert`;
    `return_to_caller`'s doc ("adjust the results ... releasing the charges
    the callee's `__call` resolution held"): released before adjusting.
  - `invoke_rust_fn`'s doc (section 2); its inline invariant comment stays.
  - `get_table_unguarded`'s inline comment; `set_table`'s and
    `tostring_bytes`'s docs (section 4).
  - Comments at the deleted inner truncations go away with them; the
    boundary arms carry the explanation.
- `docs/limits.md` "Native stack": no change unless the per-level probe
  numbers change materially (it states no per-level figures today).
- The `fault-injection` feature (Acceptance 4): `Cargo.toml`, the feature in
  `tests/common/stack.rs`'s instrumented-stack condition, and AGENTS.md's
  Tests and Commands sections (what the sweeps check, how to run them on
  demand, that the feature is never a production or probe configuration).
  It gets no `[[check]]` shape here; whether it should become standing
  infrastructure is a separate decision for the user.
- `notes/deferred.md`: record the un-deferral in the first commit (Status);
  remove the HYG-045 entry once landed.

## Acceptance

Sequencing: first the `__call` native-stack cycle and the probe (2, 3), then
the fault injector, the sweeps, the natural-failure corpus and the transcript
dump (4), and the new bench workload (6), each landing as its own commit(s)
on the unchanged VM and passing there; the probe's numbers and the baseline
transcript are recorded with commit hash and hostname. Then the boundary
change, then the bench A/B (6).

Stop conditions, so the implementer never has to judge: a sweep, focused
test or natural-failure case that fails **on the unchanged VM** is a
pre-existing bug, not a test to weaken or a fix to fold into the change
commit; stop and report it with the failing case. A probe cycle that grows
by any amount in any gated configuration (3), a bench regression that
survives the same-binary control (6), or any transcript difference (4; the
dev-only transcript has no naming noise to excuse) likewise stops the work
and goes to the user with the measurements.

1. `brokkr check` passes (both existing shapes), as do
   `brokkr check --features hotpath` and a check pass with `fault-injection`
   and `snapshot` enabled (nothing else compiles the injector's code).
2. `tests/native_stack.rs` gains a `callable_rust` cycle in `CYCLES` before
   the change: a callable object whose `__call` is a re-entering library
   function, so `begin_callable_call`'s frame is live on the cycle (no
   existing cycle has it), e.g. `local t = setmetatable({2, 1}, {__call =
   table.sort})` then `t(function(a, b) ... deep(n + 1) ... end)`, which runs
   `table.sort(t, cmp)`. The boundary change then leaves the file unchanged,
   and it passes on its 2 MiB thread in the dev profile and under
   `--features hotpath` (larger test stack). This is the safety gate: its
   scenarios exercise the full depth envelope including fixed entry overhead,
   the deepest leaf work and the error path (`refuse_one_more` fails at full
   depth and unwinds every level). It proves the enumerated scenarios fit on
   the tested build and platform, not every possible nesting of native paths.
   The 2 MiB envelope claim holds for uninstrumented builds only;
   `tests/common/stack.rs` grants the larger instrumented stack under
   `fault-injection` as it does under `hotpath`, so every limit test using it
   survives the injector's added code.
3. New `#[ignore]` probe in `tests/native_stack.rs` that prints native bytes
   per level for each cycle in `CYCLES` (the new one included): a host
   `RustFunc` called at every `deep(n)` level records the address of one of
   its locals; consecutive differences are the per-level cost. Supporting
   evidence, not the gate: it measures only the recurring per-level portion
   of each cycle, not fixed entry overhead, leaf work or error-path peaks.
   Run it with `brokkr test` in the dev profile and with `--release`, each
   with no features and with `snapshot`: the uninstrumented configurations
   the envelope is stated for. Each is compared against its own baseline,
   before (on the probe's own commit) and after the change. Release is what
   ships, and whether `call_unguarded` or `begin_callable_call` keeps more
   live across re-entry depends on release inlining. No cycle may grow in
   these four configurations; any growth, one 16-byte alignment step
   included, is a stop-and-report condition (Sequencing), never accepted on
   the implementer's own judgement. Instrumented builds
   (`hotpath`, `hotpath-alloc`, `fault-injection`) make no envelope claim:
   for them the requirement is the gate passing on their larger stack, and
   probe numbers may be recorded but do not gate. If the cycles shrink,
   raising `MAX_NATIVE_CALL_DEPTH` toward reference Lua's 200 becomes
   possible, but that is replay-visible and a separate decision for the user.
4. Tests. Existing suites are the regression guards and are named here so
   their passing is read as evidence: `tests/rust_fn_result_count.rs`
   (over-report rejected directly, from Lua, as a generic-for iterator, and
   nested, each counting only the callback's frame - so no new over-report
   test), `tests/host_table_api_atomicity.rs`
   (`get_table_preserves_the_key_on_dispatch_preflight_failures` and
   `get_table_still_consumes_the_key_when_a_handler_errors`, the main guard
   for deleting the inner truncations), and `tests/host_error_reporting.rs`
   (notify-once across `__call` chains, nested callback failures, setup and
   padding preflights, a re-returned already-reported error, and the guards
   for deleting `call_chain_depth` named in section 1). New focused tests,
   unit tests in `src/vm/tests.rs` only where they need `pub(crate)` entry
   points:
   - **Caught-and-continue recovery** (`tests/caller_stack_isolation.rs` or
     `tests/host_frame_isolation.rs`). A Rust callback catches a failed
     nested `State::call` or `State::get_table`, checks that its surviving
     arguments and stack top are exactly as before the nested call (the
     nested call's operands gone, nothing else changed), then continues and
     returns successfully. Nested failures, each separately: a simple
     callback error; a handler error inside a chained `get_table`; a failure
     after chain units were acquired (a non-function `__call` chain whose
     terminal handler errors); and a result-padding failure (stack cap) on a
     nested call reached through such a chain. The callback is itself reached
     both directly and through a non-function `__call` chain, so a live outer
     owner holds units across the inner failure. After each caught failure
     the callback shows the counters back at its own entry values through
     behavior: a further `__call` chain from inside it resolves at exactly
     the depth left at its entry and one step more fails with
     `MetamethodDepthExceeded`. The outer call then completes, and the State
     is reused. These are the only tests of recovery execution; the sweeps
     below are fail-fast.
   - Chained table `__index` and string-receiver `__index` chains that fail
     before function-handler entry (key restored, stack unchanged) and after
     it (height `top - 1`), in `tests/host_table_api_atomicity.rs` beside the
     single-level cases.
   - Callable-object chains with non-function `__call` handlers (so units
     are actually held) failing during resolution (no handler), at admission
     (call depth) and at result padding (stack cap), each reached both from
     the host (`call_unguarded`) and from Lua (`call_from_lua`). Each asserts
     the error kind, `on_error` fired exactly once, and, where a Lua frame
     exists, the fault location and traceback; then State reuse, including a
     further chain resolving to full depth. Reporting cases go in
     `tests/host_error_reporting.rs`, depth cases in
     `tests/vm_depth_regressions.rs`.
   - Lua-to-Lua return padding in one invocation: a Lua callee reached
     through a non-function `__call` chain returns fewer results than its Lua
     caller's `OP_CALL` expects, with the stack at the cap, so
     `return_to_caller`'s `finish_call` fails. Assert the fault is located at
     the caller's call line, `on_error` fired once, the units are released
     (State reuse succeeds and a further chain resolves to full depth), and a
     closure that captured a caller local still reads the value it had at the
     failure. Alongside it, the same padding failure for a Rust callback's
     results and for host-to-Lua results.
   - A nested failure while the caller holds a captured local and a live
     pending base (a dynamic-argument call such as `f(a, g())` or a
     constructor `{x, g()}` whose `g` fails through a callable chain or a
     Rust callback), followed by reading the captured local through its
     closure and running the caller's code again: covers the replacement and
     upvalue-preservation gaps the debug checks cannot see.
   - Every test above that ends at rest also runs `save_state` after
     clearing the host's stack (snapshot quiescence requires
     `metamethod_depth`, `native_call_depth` and the stack at zero), gated
     `#[cfg(feature = "snapshot")]` so it runs in the `snapshot` check
     shape. This catches leaked or double-released units.
   - **Sweep corpus.** Short scripts covering Lua calls, `__call` chains,
     `__index` / `__newindex` table and function chains, string receivers,
     generic-for over Rust and Lua iterators, library callbacks
     (`table.sort` comparator, `string.gsub` function, `table.insert` on a
     `__newindex` proxy), and a `__tostring` handler reached through
     `tostring` and through the host's `tostring_bytes` (section 4). The
     corpus is declared **fail-fast**: no Rust
     callback in it catches an error and continues (dellingr has no
     `pcall`), every case starts from a fresh State with a valid dispatch
     (host-misuse preflights are outside the sweep), and caught-error
     sequences stay in the focused tests above. Every case's tested entry is
     `State::call`, so the expected host stack after any run follows from
     that one contract, whichever inner event failed: after `Err`, exactly
     the values below the callee slot, unchanged; after `Ok`, those plus the
     adjusted results. Preflights deeper down (`call_anchor_unguarded`'s
     `check_stack_space(1)`, `tostring_rendering`'s `check_stack_space(2)`,
     `table_sort_less`'s `check_stack_space(3)` and the like) sit under that
     boundary and need no per-event expectation; a case whose tested entry
     were anything else would have to declare that entry's contract. Every
     sweep run, after the run: the host's stack top and surviving values are
     as derived; if `State::call` returned `Err`, `on_error` fired **exactly
     once** for the tested call and its rendering equals the returned
     error's; on `Ok` it fired zero times (the host.rs contract is once per
     error, not once per `Err` outcome: a re-returned already-reported error,
     a host-misuse preflight and a caught nested failure all break a
     per-outcome rule, which is why the corpus restrictions exist); the State
     is reusable; and in snapshot builds `save_state` succeeds after clearing
     the stack. Independent of the transcript below, these assertions are the
     durable gate.
   - **Budget sweep.** Each corpus case is run once unbudgeted to get its
     cost, then with `set_cost_budget(b)` for every `b` from 0 to that cost,
     the pattern `tests/cost_check_interval.rs` uses; the result must be `Ok`
     or `BudgetExceeded`. A finite budget makes the dispatch loop flush
     before pending cost would exhaust it (`cost_batch_needs_flush`), so this
     reaches every reachable positive-cost admission point, native library
     charges through `CostMeter` included. It does not fail free structural
     opcodes (nothing can, through a budget), and multi-unit charges repeat
     an outcome across several budgets. The injector cannot replace it: an
     unbudgeted run batches cost and flushes only every
     `COST_CHECK_INTERVAL` units, and some native accounting bypasses
     `charge_cost`.
   - **Fault injector.** A non-default cargo feature `fault-injection` (a
     `debug_assertions` hook would add code to `check_stack_slot`, which is
     `#[inline(always)]` into callers on native re-entry cycles, growing the
     default dev frames that `tests/native_stack.rs` pins and the probe
     measures). It gets **no** `[[check]]` shape: `tests/diff_test.rs` runs
     the whole differential corpus once per shape, so a third shape would add
     a full clippy, test and diff pass to every `brokkr check` for the life
     of the project. Its sweeps are `#[ignore]` tests, run on demand with
     `fault-injection` and `snapshot` enabled (so the `save_state` checks run
     too), on the unchanged VM and again on the change; the result is
     recorded in the change commit's message. Whether to make it standing
     infrastructure is the user's separate decision (section 7).
     The injector is a countdown and event log on `State`, armed by a
     hidden test API after setup. `check_stack_space` and `check_stack_slot`
     take `&self` and are called from contexts holding shared borrows, so
     the countdown is a `Cell<u32>` and the event log a `RefCell<Vec<_>>`
     (both `cfg(feature = "fault-injection")` fields), decremented and
     appended through interior mutability from inside those checks; the
     hooks do **not** change any preflight's signature to `&mut self`. The
     `RefCell` borrow is taken and released inside the hook, which calls
     nothing, so it cannot be held across re-entry. It fails the Nth call,
     counted across all of them, of: `check_stack_space` (zero-growth calls
     included, since
     admission's `check_stack_space(0)` is a real exit site),
     `check_stack_slot`, the call-depth admission check, the native-depth
     check in `call_unguarded`, the `__call` chain metamethod-depth check and
     `with_metamethod_depth`'s check. It fires at the existing refusal
     branch, before the mutation the check protects, with that primitive's
     real error kind; it never fires inside operations documented as
     infallible after a successful preflight. It fires once, then disarms
     before cleanup checks, snapshot validation and reuse. Recording an
     event calls no fallible State API. Each run records the event's
     identity and arguments and whether it fired, so `Ok` cannot hide an
     unreached target and a future change in call order cannot silently
     compare different fault sites. Sweep: run each corpus case once armed
     with no target to count M events, then for every N in 1..=M.
     Coverage is limited to the refusals on that recorded fail-fast
     execution; checks reached only after a caught error (recovery
     execution) are not enumerated, and are covered only by the focused
     caught-and-continue tests above.
     The native-depth check is **eligible only when `native_call_depth >
     0`**: with an outer bracket active, a second `call_unguarded` is reached
     only from a running Rust callback or a Lua activation (chain resolution
     calls nothing), so `is_outermost_host_boundary` is false and the new
     error arm's added `report_call_error` is a no-op, as it is for every
     real refusal (the limit is far above 0). An outermost synthetic refusal
     would notify after the change and not before, a synthetic-only
     difference; skipped outermost checks are recorded as such.
     Why synthetic failures are sound here: cleanup at an exit depends on
     the site, not on why its check failed, so failing a check that real
     heights could not fail in that order still tests the boundary rule.
     What they show is resilience to those refusals, not that each exact
     failure sequence is naturally reachable.
     A stack-headroom sweep cannot replace the injector: with `k` free slots
     a run fails at the first check whose required height exceeds `k`, so
     only checks that set a new prefix maximum of required height can ever
     be failed; padding in `finish_call` after a taller callee frame, or
     `__call` argument insertion after a deeper earlier call, is shielded at
     every `k`.
   - **Natural-failure cases** for exits the injector does not reach,
     because they are not structural resource refusals: `TooManyArguments`
     during `__call` receiver insertion and dynamic-argument resolution,
     `TooManyResults` during return collection, Rust callback `Err` and
     `ResultCountExceedsStack`, and type, argument, pattern and library
     errors. String size is covered here too, not by injection:
     `check_string_size` and `checked_string_growth` are free functions
     shared with the lexer, and `append_bytes_units` in
     `src/lua_std/string.rs` uses `checked_string_growth` as a predicate
     whose failure switches to a byte-at-a-time fallback that runs checks
     absent from the uninjected trace, so a single-fault sweep would neither
     be well-defined there nor reach the fallback. Cases crossing
     `MAX_STRING_BYTES` naturally: concat, `string.format`, `string.gsub`
     with a string replacement whose crossing happens in a literal segment
     on a later application (asserting the size-error payload and the
     charged prefix the fallback produces, so the case provably takes the
     fallback), `string.gsub` with a function replacement, and an oversized
     `push_string` inside a Rust callback (`alloc_string`'s own refusal).
     Each gets the corpus post-run checks.
   - **Release runs.** The configuration that ships gets the same
     error-path validation without the debug checks: the focused tests, the
     budget sweep and the natural-failure cases also run with
     `brokkr test --release`, with no features and with `snapshot` (so the
     `save_state` checks run). The injector sweeps stay dev-only (a
     `fault-injection` build is never a release configuration).
   - **Transcript A/B (supporting evidence for Replay visibility, one
     configuration).** An `#[ignore]` dump test writes a transcript of every
     budget-sweep and natural-failure run to `.scratch/`. It runs on the
     parent commit and on the change commit, same toolchain, in **dev with
     no features** only, and the before and after transcripts must be
     byte-identical; the result is recorded in the change commit's message.
     One configuration is enough because the durable gate is the assertions
     above, and the other configurations add nothing the gate does not
     already check: the `snapshot` shape's `save_state` bytes are asserted by
     the `#[cfg(feature = "snapshot")]` tests themselves; the injector's
     before/after evidence is its own sweep passing on both commits; and a
     release transcript would carry the address-based callback-naming noise
     `reference/error-reporting.md` describes (link-time optimization and
     identical-code folding can change which name an anonymous callback gets
     between builds), which the release test runs below cover without a
     byte comparison. Not a committed fixture: it is evidence for this
     change, and the durable gate is the assertions above. Per run, in this
     fixed observation order so that observing cannot disturb what is
     observed: the returned error's rendering and its structured kind,
     source, position, traceback and `vm_fault`; every notification from the
     tested call, in order; then, through non-executing raw reads, the host
     stack top, surviving caller values and each case's designated
     observables (captured locals, mutated globals); `cost_used`; clear the
     stack; only then the reuse workload, with its notifications delimited
     from the tested call's. `Display` alone is lossy, hence the structured
     fields.
5. `./scripts/diff_test.sh` prints `ok`.
6. Wall-clock: interleaved A/B with `brokkr dellingr --lua <W> --bench`
   against the parent commit, medians and sign consistency across pairs,
   plus the five-launch same-binary control before trusting a small delta.
   Workloads: `method` and `benchmark` (registered today; between them they
   exercise `OP_CALL` into Lua and Rust callees), and a new registered
   workload dominated by calls made from Rust (an `__index` function
   handler, a `table.sort` comparator, `string.gsub` with a function, a
   generic-for over a Lua-closure iterator, and a callable object), since the
   change touches `call_unguarded` and `begin_callable_call` as much as
   `call_from_lua` and no registered workload exercises those paths. Register
   it (with its `bench/` port and both pins) before the change commit so
   `--bench --commit <parent>` can baseline it. `begin_call` inlines into the
   dispatch loop's call path, so a regression there is a blocker. The change
   removes work from that path (no chain-counter access on function callees,
   no error arm), so a regression is expected only from layout; if one
   survives the same-binary control and its cause cannot be fixed, stop and
   bring the measurements to the user. There is no partial-shipping
   fallback: chain ownership lives in `begin_callable_call`, shared by both
   call paths, so the two sides cannot be split coherently.

## Replay visibility

None intended. Error kinds, messages, positions, tracebacks, `on_error`
count, cost charged, and the stack a host observes after an error are all
unchanged; the new tests pin the last and, for every chain case, the
location and traceback; the existing cost goldens and `diff_test.sh` pin
cost and rendering; and the dev-no-features transcript A/B in Acceptance 4
shows all of them together over the sweep corpus as supporting evidence.
The point at which `on_error` fires moves for a Rust function the host calls
directly (from `invoke_rust_fn` to `call_unguarded`'s arm, after truncation),
which `on_error` cannot observe: it receives only the `Error` on the callbacks
object. Deleting `call_chain_depth` changes no saved bytes (it was never
serialized) and no `save_state` outcome (it was zero in every quiescent
State).

## Review disposition

### Settled in earlier rounds

Still binding, and folded into the body above: debug bookkeeping must not
land on native frames that are live across re-entry; the probe lands first,
is supporting evidence only, and `tests/native_stack.rs` is the gate;
`invoke_rust_fn`'s error-path truncation and reporting are redundant (its
`stack_bottom` restore is not, fifth round); reporting behavior (count,
location, traceback) is pinned, not only cleanup; Lua-to-Lua padding through
`return_to_caller` gets its own case; length and upvalue assertions prove less
than they appear to; no injection on `charge_cost` (the budget sweep covers
costed steps more completely) or on allocation (object allocation is
infallible; string size is covered by natural cases); the outermost
native-depth check is not injected; notification is once per error, not per
`Err` outcome; the transcript A/B with its fixed observation order. A
quiescence check at every `State::guarded` return was rejected (inside a
callback the State is legitimately not at rest); this round's at-rest check is
gated on the outermost call for that reason.

### Fourth round: accepted

Review 1 (never committed; summarized here):

- 1, the watermark trades a local count for a global invariant and adds a
  load and a computed delta to every `OP_CALL`: verified (`call_from_lua`
  would read `call_chain_depth` on entry, and today's `finish_call` already
  runs `release_chain_charges` on every Rust callee). Adopted the local owner
  in `begin_callable_call`, the transfer on admission for both call kinds,
  and release on a Rust callee's `Ok` (section 1). The inner resolution
  function the review suggests is constrained to an inlined body or a
  labeled loop, since `begin_callable_call` is live across native re-entry.
- 2, `call_chain_depth` becomes redundant: verified. Its only reader is
  `is_outermost_host_boundary`; after the change the only `report_call_error`
  call is `call_unguarded`'s arm, where outer units imply `call_depth > 0` or
  `active_rust_fn` set. Deleted (section 1), with the field's documented
  reason for existing checked against the new predicate.
- 3, `call_from_lua` needs no error arm: verified that nothing between its
  `Err` and `unwind_invocation` reads the stack above the frame
  (`Frame::run`, `report_frame_error`, `build_stack_trace`, `on_error`).
  Arm removed (section 1). The fifth round made the `stack_bottom` half of
  that argument unnecessary by keeping `invoke_rust_fn`'s restore.
- 4, the `BoundaryMark` stack is disproportionate once 1 and 2 land: adopted.
  Replaced by local checks in cold code and an outermost-gated at-rest check
  at `State::guarded` (section 6). Two corrections to the review's version,
  found while validating: the at-rest set must exclude
  `transient_roots.suspended_envs`, since `with_restricted_env` runs host
  closures that call guarded entry points while its saved environment is
  legitimately held (`TransientRoots::is_empty` includes it, so "the
  `validate_quiescent` set minus the stack" would fire on correct code); and
  the gate must not be evaluated from `call_depth` or `native_call_depth`
  after return, since a leak would then disable the check that should catch
  it. `active_rust_fn` after return is sound because `invoke_rust_fn`
  restores it unconditionally. Also confirmed: `validate_quiescent` is
  snapshot-gated and its macro has no per-item `cfg`, so the list must be
  generated from an ungated inventory.
- 5, no native-stack cycle has `begin_callable_call`'s frame live: verified
  (`CYCLES` has no `__call` cycle). `callable_rust` added before the change
  (Acceptance 2, 3).
- 6, the probe matrix exceeds the invariant: verified against
  `tests/common/stack.rs` and `docs/limits.md`, which state the envelope for
  uninstrumented builds only. Gating narrowed to no features and `snapshot`,
  dev and release (Acceptance 3).
- 7, a permanent `fault-injection` check shape taxes every `brokkr check`:
  verified (two shapes today; `tests/diff_test.rs` runs the corpus per
  shape). The injector runs on demand; the shape is the user's separate
  decision. Its oracle gap is closed: every sweep case enters through
  `State::call`, so the expected host stack follows from that contract alone
  (Acceptance 4).
- 9, `set_table`'s arm is untestable defensive code: verified (both callers
  in `src/lua_std/table.rs` propagate). Dropped; the doc comment states the
  error contract instead (section 4).
- 10, benchmark scope: `calls/global` / `calls/general` registration
  dropped. With 1 and 3 the `OP_CALL` path loses work; `method` and
  `benchmark` cover it, and the new Rust-call workload covers what is
  genuinely missing (now including a callable object).
- 11, document hygiene: the earlier review citations pointed at files that
  were never committed and whose paths these reviews now occupy; the old
  per-problem disposition is replaced by the summary above. History cut to
  what is still binding. The un-deferral is recorded in `notes/deferred.md`
  by the first commit (Status).

Review 2 (never committed; summarized here):

- 1, the performance fallback is underspecified against the debug ownership
  scheme: resolved by removing the fallback. With chain ownership in
  `begin_callable_call`, shared by both call paths, the sides cannot be split
  coherently; a surviving regression goes back to the user (Acceptance 6).
- 2, the shipping configuration needs its own error-path testing: adopted
  (Acceptance 4, release runs; the release transcript was dropped in the
  fifth round, the release test runs stay). Its motivating
  hazard, a paired leak absorbed by the watermark in release, no longer
  exists, but release still runs none of the debug checks, so direct testing
  stands on its own.
- 3, the injector's coverage claim must separate recorded execution from
  recovery execution, and recovery tests must exercise failures after chain
  acquisition and at result padding with named surviving operands and
  counters: adopted (Acceptance 4, caught-and-continue recovery; coverage
  sentence in the injector bullet).

### Fourth round: rejected

- Review 1, 8, make the transcript A/B optional and non-gating. Rejected in
  the fourth round on the ground that the transcript was the only corpus-wide
  evidence for locations, tracebacks, rendering and `cost_used`; the fifth
  round narrowed it instead (below): the focused tests pin location and
  traceback for every chain case and the goldens pin cost and rendering, so
  the transcript is supporting evidence in one configuration, still
  byte-identical and still recorded.
- Review 1, 4, "nothing new lands on native frames, release or dev" as a
  property of the proposed check. Narrowed, not rejected outright: it holds
  only if the gate reads no entry value, which is why the gate is
  `active_rust_fn` after return (section 6) rather than a recorded entry
  state.
- Review 2, closing remark, a small executable model of charge ownership.
  Not taken: with one local owner per phase, each owner's correctness is
  checkable in place, and a model would restate the code it is meant to
  check. The focused chain tests and the injector sweep exercise every
  transition.

### Fifth round: accepted

One review (Claude, read-only, 2026-10-22, validated against 6a49df8 with
the fourth-round text), which verified the load-bearing claims against the
code (no catch-and-continue site in `src/lua_std/` or `src/vm/`; nothing
between an `Err` and its unwind reads stack values; the
`is_outermost_host_boundary` argument; the `get_table_unguarded` height
equivalence; the native-depth no-op; `fail_call`'s upvalue check) and asked
for seven changes, all taken:

- 1, keep `invoke_rust_fn`'s `stack_bottom` restore on `Err`: the function
  that moves it moves it back, as admission already does; one store in cold
  code removes two "nothing in between reads `stack_bottom`" proof
  obligations that were true but would break invisibly (section 2, ownership
  table, section 1's `call_unguarded` and `call_from_lua` paragraphs,
  section 6's `fail_call` check).
- 2, the injector's countdown must use interior mutability, since
  `check_stack_space` and `check_stack_slot` are `&self` (Acceptance 4,
  injector bullet).
- 3, the at-rest check body goes in a cold out-of-line helper so the generic
  `guarded` frame does not grow (section 6).
- 4, the transcript A/B runs in dev with no features only, as supporting
  evidence; the release and snapshot test runs stay (Acceptance 4, Replay
  visibility).
- 5, explicit stop-and-report conditions for an unattended implementer
  (Acceptance, Sequencing; Acceptance 3).
- 6, the Status cited review files that exist neither in the tree nor in
  git history (6a49df8 added only the spec); citations replaced by "never
  committed; summarized here".
- 7, `tostring_bytes` is a guarded entry that runs user code and was missing
  from the boundary inventory; added with `call_anchor` as entries that ride
  on `call_unguarded`'s arm (The rule, ownership table, section 4, section
  7, corpus).
