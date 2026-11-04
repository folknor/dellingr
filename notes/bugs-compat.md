# Compatibility findings: divergences missing from docs/compatibility.md

Filed from the 2026-11-04 defect hunt (scopes `compiler-front`, `vm-dispatch`,
`stdlib-strings`, `stdlib-core`). Each entry is a behaviour that differs from
lua5.2 or lua5.4 and is not listed on the page that says "Every deliberate
divergence from Lua 5.2 and 5.4 ... A difference not listed here is a bug."
Resolving one means adding it to the page, or changing the code if the
divergence was not deliberate. Hunt reports are work documents and may be wrong;
verify before fixing.

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

## COMPAT-001 - No proper tail calls, and the divergence is listed nowhere

Scope: `vm-dispatch`. Hunter's label: medium. Replay-visible either way: it
changes which scripts succeed.

Claims broken: `docs/compatibility.md`'s opening sentence; neither that page nor
the README's "Won't implement" list mentions tail calls.

The instruction set has no tail-call opcode (`instr.rs` `opcode_rows!` ends at
`OP_CALL`, `OP_RETURN`, `OP_VARARG`, `OP_CLOSURE`). `return f(x)` compiles to
`OP_CALL ... RetCount::All` followed by `OP_RETURN All`. The caller's activation
record and frame stay live for the whole callee (`State::admit_lua_call` pushes
a `CallInfo`; `Frame::eval_batched` pushes a frame on `Leave::Call`), and every
level counts against `MAX_CALL_DEPTH` (1000). Both references guarantee proper
tail calls, so tail recursion runs at constant depth there.

Effect on script authors: idiomatic tail-recursive code works in lua5.2 and
lua5.4 and dies here with `CallDepthExceeded` after 1000 levels. Examples:
state machines (`return state_b(ctx)`), CPS loops, and
`local function loop(n) if n == 0 then return end return loop(n - 1) end
loop(5000)`. Errors kill the callback, so this is a hard failure. Second
effect: a `__call` chain charge held in a tail-calling activation's record
(`CallInfo::chain_charges`) stays held until the whole tail chain unwinds, so
callable-object tail recursion also hits `MAX_METAMETHOD_DEPTH` (200).
Tracebacks also differ, since Lua prints `(...tail calls...)`.

Direction the hunter gave: the dispatch loop already has what an `OP_TAILCALL`
needs, since frames live on the invocation's own `Vec<Frame>` and admission is
separate from execution. A tail call would close the frame's upvalues, slide
the callee and its arguments down into the current function slot, pop the
current activation (carrying its chain charges into the new record or releasing
them), admit the callee in place, and replace the top frame instead of pushing
one. `pending_call_results` keeps working because the caller's suspended
`OP_CALL` still decides the result count; a Rust callee in tail position can
stay an ordinary call followed by a return. If tail calls stay unsupported on
purpose, they belong in the README's "Won't implement" list and in
`compatibility.md`, with the depth consequence. The hunter notes that merging
`CallInfo` into `Frame` (tracked in `notes/optimizations.md`, call path after
the frame-stack flattening) would make the in-place tail call simpler.

## COMPAT-007 - Unlisted base and table library divergences: select and empty sort

Scope: `stdlib-core`. The other bullets of this entry are documented; these two
were left alone at triage.

- `select` treats only the exact string `"#"` as the count form. lua5.2 and
  lua5.4 test only the first byte (`*lua_tostring(L, 1) == '#'`), so
  `select("#n", ...)` returns the count there and raises "number expected, got
  string" here.
- `table.sort` type-checks a non-nil second argument even when the length is 0
  or 1. Lua 5.4 checks the comparator only when `n > 1`, so
  `table.sort({}, 42)` succeeds there and raises here.
