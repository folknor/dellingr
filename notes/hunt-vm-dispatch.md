# Hunt: vm-dispatch

Scope: `src/vm/eval.rs`, `eval_control.rs`, `eval_index.rs`, `eval_store.rs`,
`frame.rs`, `registers.rs`, `stack.rs`, `src/cost_meter.rs`,
`src/vm/interrupt.rs`, `src/limits.rs`, `src/limits/tuning.rs`. I traced
values out into `metamethod.rs`, `table.rs`, `vm.rs`, `compiler/verify.rs`,
`compiler/codegen.rs` and `lua_std.rs` where the in-scope code depends on
them.

Findings are ordered by impact. Each one names the claim it breaks.

---

## 1. No proper tail calls, and the divergence is listed nowhere (medium)

**Claim broken:** `docs/compatibility.md`, first paragraph: "Every deliberate
divergence from Lua 5.2 and 5.4. A difference not listed here is a bug."
Neither that page nor the README's "Won't implement" list mentions tail
calls.

**What the code does:** the instruction set has no tail-call opcode
(`instr.rs` `opcode_rows!` ends at `OP_CALL`, `OP_RETURN`, `OP_VARARG`,
`OP_CLOSURE`). `return f(x)` compiles to `OP_CALL ... RetCount::All` followed
by `OP_RETURN All`. The caller's activation record and frame stay live for
the whole callee (`State::admit_lua_call` pushes a `CallInfo`, and
`Frame::eval_batched` pushes a frame on `Leave::Call`), and every level counts
against `MAX_CALL_DEPTH` (1000). Both references guarantee proper tail calls,
so tail recursion there runs at constant depth.

**Effect on the script author:** idiomatic tail-recursive code works in
lua5.2 and lua5.4 but dies here with `CallDepthExceeded` after 1000 levels.
Examples are state machines (`return state_b(ctx)`), CPS loops, and
`local function loop(n) if n == 0 then return end return loop(n - 1) end
loop(5000)`. Errors kill the callback, so this is a hard failure. There is a
second effect: a `__call` chain charge held in a tail-calling activation's
record (`CallInfo::chain_charges`) stays held until the whole tail chain
unwinds, so callable-object tail recursion also runs into
`MAX_METAMETHOD_DEPTH` (200). Tracebacks also differ, since Lua prints
`(...tail calls...)`.

**Direction:** the dispatch loop already has what a real `OP_TAILCALL` needs.
Frames live on the invocation's own `Vec<Frame>`, and admission is separate
from execution. A tail call would:
- close the frame's upvalues;
- slide the callee and its arguments down into the current function slot;
- pop the current activation, carrying its chain charges into the new record
  or releasing them;
- admit the callee in place, and replace the top frame instead of pushing a
  new one.

`pending_call_results` keeps working, because the caller's suspended
`OP_CALL` still decides the result count. A Rust callee in tail position can
stay an ordinary call followed by a return. If tail calls stay unsupported on
purpose, they belong in the README's "Won't implement" list and in
`compatibility.md`, together with the depth consequence.

This is replay-visible either way: it changes which scripts succeed. Call it
out.

---

## 2. A Rust call is not a safepoint, so the documented bound on work between safepoints is false (low-medium)

**Claim broken:** `src/vm/interrupt.rs` module doc says "Between two
safepoints the work is at most one linear pass over a string capped by
`MAX_STRING_BYTES` or over a table's storage." `docs/budget.md` ("Stopping a
script from the host", Safepoints bullet) repeats it.

**What the code does:** the safepoints are:
- backward jumps (`Frame::branch`, `instr_for_loop`);
- Lua admission (`admit_lua_call`);
- the library loops listed in `interrupt.rs`.

None of these covers calling a Rust function. `call_from_lua`, `begin_call`,
`begin_callable_call`, `invoke_rust_fn` and `instr_tfor_call_rust_fn` never
poll. Straight-line bytecode can therefore chain any number of Rust calls,
each doing a full linear pass, with no safepoint in between. For example,
after a 16 MiB string `s` is built, a generated chunk of nothing but
`s:upper()` lines can do the following without one poll:
- `MAX_SOURCE_BYTES` is 16 MiB, so the chunk holds about 1.6 million calls;
- each call makes a 16 MiB pass, an allocation and an intern hash.

The same holds for repeated `string.lower`, `reverse`, `sub` or `format`, and
for Rust `__index` / `__newindex` / `__len` handlers reached from
straight-line code. Work stays bounded by code size, so "no unbounded
execution" survives. The "at most one linear pass" bound does not. With no
budget (the count-only default, and the CLI default) or under
`Costing::Off`, an editor's stop button cannot stop such a script for a very
long time.

A smaller instance of the same claim: an allocation-triggered collection
(`State::allocator` calls `maybe_collect`) is a pass over the whole heap,
which is neither a string nor one table's storage, and it can happen between
any two safepoints.

**Direction:** poll once in `invoke_rust_fn` before calling `f`. It is the
single choke point for every Rust callee, and the poll is one relaxed load.
The interrupt would then be reported at the caller's call instruction, like
an admission refusal. The alternative is to restate the bound as "per
library call" and drop the stronger wording.

---

## 3. The docs name the wrong error for over-wide dynamic calls (low)

**Claim broken:** `docs/limits.md` ("Runtime call width") says
"`f(table.unpack(t))` or `f(...)` with more values raises a too-many-arguments
error". The `MAX_ARGUMENTS` doc in `src/limits.rs` says the same, and the
`MAX_RESULTS` doc says the same about `return ...`.

**What the code does:**
- `table.unpack` / `unpack` refuses a span of 256 or more itself, with
  `TooManyResults` (`lua_std.rs`, `unpack_values`). So `f(table.unpack(t))`
  raises too-many-*results*, from unpack, before `call_window` sees a count.
- `f(...)` can never exceed the cap. A function's varargs are already capped
  by the argument cap at its own entry (`call_window` /
  `u8::try_from`), so `...` holds at most 255 values. The same holds for
  `return ...`.
- `TooManyArguments` actually comes from a fixed prefix plus an open tail
  (`f(x, ...)`, `f(x, table.unpack(t))`; `call_window`, `ArgCount::Dynamic`)
  and from a `__call` receiver pushed past 255 (`begin_callable_call`).

A script author who follows the docs looks for the wrong error kind.

---

## 4. The `Frame` invariant overstates the stack length (low, documentation)

**Claim broken:** the doc comment on `struct Frame` in `frame.rs` says "While
its bytecode runs the stack is at least `top` long; it is exactly `top`
except between an instruction that leaves open results and the one that
consumes them...".

**What the code does:** an open producer can leave the stack *shorter* than
`top`:
- `reg_vararg` with `RetCount::All` truncates to `dst` and appends however
  many varargs exist, possibly none;
- an `OP_CALL` with `RetCount::All` leaves `func_slot + count`.

The verifier's open-results protocol (`verify.rs`, `produces_open` /
`consumes_open`, `open_window_fits`) makes this safe, because only a consumer
can run next. But the unconditional "at least `top`" sentence is false. A
future handler that pushes at `stack.len()` would land inside the frame's
registers if it ever ran between a producer and its consumer.
`instr_tfor_call` and `resolve_index` are examples of handlers that push
there. Reword to "exactly `top`, except between an open producer and its
consumer, when it may be shorter or longer".

---

## 5. Stale vocabulary in comments (cosmetic)

- `eval.rs`, `call_unguarded`: "ArgCount::Dynamic is resolved by the
  executing Lua Frame against its own pending base. Hosts never own such a
  base". The register VM has no pending base. `call_window` derives the
  count from `stack.len()`.
- `reference/error-reporting.md` (Panics and poisoning) lists "pending bases"
  among the state a panic leaves stale. The same leftover from the stack VM.
- `eval.rs`, `invoke_rust_fn` doc: "Every RustFn call - `call_op` and the
  generic-for fast path alike". No `call_op` exists any more. The callers are
  `begin_call`, `begin_callable_call` and `instr_tfor_call_rust_fn`.

---

## Lateral findings (outside the scope)

### L1. `docs/determinism.md` "Error labels" contradicts the naming contract (low-medium, documentation)

`docs/determinism.md` says generic Rust callbacks "get a label from the
current global bindings or, with the `snapshot` feature, from the registered
function id. The label can therefore differ between feature sets or builds,
and when globals are rebound."

`reference/error-reporting.md` says the opposite: "Nothing is discovered at
error time. The name does not depend on which globals exist ... or on the
`snapshot` feature". The code follows `error-reporting.md`: commit `912c2d5`,
"Name host functions at creation instead of discovering names from globals".
The determinism page is stale and should be reworded to the
creation-time-name rule.

### L2. Unlisted arithmetic and loop divergences (low)

- `%` with an infinite divisor follows 5.4 (`lua_modulo`: `1 % math.huge` is
  `1`, `-1 % math.huge` is `inf`). Lua 5.2 computes `a - floor(a/b)*b` and
  gets `nan`. `compatibility.md` does not list this.
- `n % 0` with whole-number operands is `nan` here. Lua 5.4 raises
  "attempt to perform 'n%%0'". This is arguably implied by "Numbers are f64
  only", but not stated.
- A numeric `for` whose control crosses 2^53 never terminates here, because
  `var += step` stops changing (for example
  `for i = 2^53 - 1, 2^53 + 1 do`). Lua 5.4's integer loop runs exactly three
  times; Lua 5.2 also hangs. `compatibility.md` mentions the 2^53 effect only
  for table keys.

### L3. Performance and structure (already partly tracked)

- Each Lua call clones four `Arc`s and each return drops them. They are the
  `CallInfo` bytecode clone in `lua_function_bytecode`, plus bytecode,
  runtime and upvalues in `admitted_frame`, all atomic operations. The closure
  sitting in the function slot already roots all of them. `CallInfo` and
  `Frame` also duplicate `bytecode` and `ip` across two parallel stacks.
  `notes/optimizations.md` already tracks both. Merging the activation record
  into the frame (one stack, records owned by invocations, tracebacks walking
  frames) would remove the duplication and the bumps together. It would also
  make finding 1's in-place tail call simpler.
- `State::pop_n` pops one value per loop turn, and `balance_stack` pushes one
  nil per turn. `truncate` and `resize` do the same work in one call.

---

## Checked and found consistent

None of these produced a finding:

- **Cost batching and the per-op admission rule.** `add_cost!` flushes before
  pending cost can exhaust a finite budget. That keeps
  `remaining - pending > 0` between flushes, so a batched settle admits
  exactly what per-op charging would. Every call out of the dispatch loop
  flushes first: `OP_CALL`, `OP_TFOR_CALL`, the `resolve_index`,
  `walk_newindex` and `resolve_length` handler paths, and `OP_RETURN`.
  Nothing in the dispatch path charges natively in the middle of a batch.
  `Frame::run`'s charge on error is admitted for every error except
  `BudgetExceeded`, as its comment says.
- **Error cleanup.** Admission refusal (`reject_admission` restores the
  activation, varargs and stack_bottom), `fail_call` truncation, unwinding
  (upvalues closed before slots, chain charges released in
  `pop_activation`), `__call` chain-charge ownership on every arm of
  `begin_callable_call`, and metamethod units in `index_call` /
  `newindex_call` are all consistent.
- **Stack cap.** Every growth path is preflighted:
  - the admission reservation with `FRAME_HEADROOM`;
  - the `__call` receiver insert;
  - the TFOR and handler windows;
  - `reg_vararg` All, before it touches registers.

  `return_to_caller`'s `resize(top)` grows only within the caller's
  admission-time reservation.
- **Interrupt safepoints.** Every backward transfer polls before the cursor
  moves. Admission polls after the depth check and before the reservation, as
  `docs/budget.md` orders them.
- **Call-site recording.** Every path that can call out records the call site
  first, so `report_escaping` and `host_print` read correct innermost records.
