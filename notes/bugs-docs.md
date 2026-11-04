# Documentation findings: docs, reference pages and doc comments that contradict the code

Filed from the 2026-11-04 defect hunt. Compatibility-page omissions are filed
separately in `notes/bugs-compat.md`. Hunt reports are work documents and may be
wrong; verify before fixing.

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

## DOC-001 - Three host-facing docs still describe the removed name lookup through globals

Raised independently by `vm-state` (fullest account), `vm-dispatch`, `vm-heap`
and `stdlib-core`.

The code (`State::active_rust_function_name` returns `self.rust_fns.name(..)`,
`vm/rust_fn.rs`), the README ("argument errors raised while it runs name it
(`game.spawn`), however the script reaches it") and
`reference/error-reporting.md` ("Argument error names": "Nothing is discovered
at error time. The name does not depend on which globals exist ... or on the
`snapshot` feature") all say an argument error's function name is the entry's
own diagnostic name, fixed at creation. Commit 912c2d5 ("Name host functions at
creation instead of discovering names from globals") made that change. Three
docs still describe the old mechanism:

- `src/error.rs`, `ArgError` doc: "the VM looks the callback up only then: first
  the first current global bound to the same function pointer, then, with the
  `snapshot` feature, the callback's registered stable id. Callbacks found by
  neither remain anonymous. The name ... can differ between builds."
- `src/host.rs`, `HostCallbacks::on_error` doc: "Generic Rust callback checks can
  resolve a label from current globals or, with `snapshot`, a registered
  function id; aliases and build settings can affect the result."
- `docs/determinism.md`, "Error labels": generic Rust callbacks checked with
  `State::check_any`/`State::check_type` "get a label from the current global
  bindings or, with the `snapshot` feature, from the registered function id. The
  label can therefore differ between feature sets or builds, and when globals
  are rebound."

All three are wrong today: function pointers are never compared, the name does
not depend on globals or features, and a host function is never anonymous while
it runs. Two binding documents now contradict each other, and the rustdoc on
`ArgError` and `on_error` is the first place a host author looks.

Fix: reword all three to the reference rule: the name is the entry's diagnostic
name (from `add_rust_fn`, `register_rust_fn`, `set_global_named_rust_fn`, or the
catalog id), fixed at creation, the same in every build and feature set, and
diagnostic text rather than an identifier. determinism.md can drop the "differ
between feature sets or builds" bullet; the only remaining cross-build variation
is a host renaming its own entries.

## DOC-002 - reference/error-reporting.md "Panics and poisoning" lists a guard that does not exist and misses one that does

Raised by `vm-state`; `vm-dispatch` independently flagged the "pending bases"
phrase.

- It says marking happens "in the unwind guards of `with_rooted_values`,
  `with_restricted_env` and the vararg frame". No vararg-frame guard exists. The
  only `catch_unwind` sites that poison are `invoke_rust_fn`,
  `with_rooted_values`, `with_restricted_env` and `State::guarded`; varargs live
  in `InFlight::vararg_stack` with no guard of their own.
- Its list of guarded entry points ("`call`, `call_anchor`, `get_table`,
  `to_string_with_meta`, `tostring_bytes`, `with_restricted_env`") omits
  `set_table`, which is guarded (`table_ops.rs`) and was added in 0a76dbc.
- "the call stack, stack bottom, pending bases, transient roots and open
  upvalues": "pending bases" names no current State field; it is a leftover of
  the stack VM.

`reference/` is the binding tier. Fix: list the four real guard sites, add
`set_table` (or, more durably, say "every public entry point that can execute
Lua; `scripts/poison_contract.py` enforces the prologue" instead of
enumerating), and drop "pending bases". The `vm-state` hunter names a
structural cause (the guard list is a convention rather than a type), filed in
`notes/bugs-host.md`.

## DOC-003 - State::is_poisoned says the cost budget stays usable; set_cost_budget refuses

Scope: `vm-state`.

`is_poisoned` doc: "Infallible host controls and observations, such as the cost
budget, user data, counters and this method, remain available."
`set_cost_budget` is fallible, starts with `check_usable()`, and returns
`StatePoisoned` on a poisoned State; its own doc says so ("the usual refusal of
a poisoned State"). Only `cost_used` and `cost_remaining` stay available. Fix:
say "cost observations", or list the real survivors: `cost_used`,
`cost_remaining`, user data, GC controls, `seal_globals`, `set_interrupt`,
`release_anchor`, counters.

## DOC-004 - docs/limits.md limits never-reclaimed Rust function entries to snapshot registrations

Scope: `vm-state`.

`docs/limits.md`, "Never reclaimed": "with the `snapshot` feature, Rust
function registrations persist while unreachable, one per registered id." The
function table (`RustFnTable`) is append-only in every build: every
`add_rust_fn` call, which needs no feature and takes no id, adds an entry that
lives as long as the State. The doc comments on `State::add_rust_fn` and
`RustFnHandle` say so ("Entries live as long as the State ... creating one per
call grows the State's function table for its whole life"). The limits page is
where a host looks for unbounded growth, and it states a narrower case. Fix:
"Never reclaimed: every Rust function entry (`add_rust_fn`, `register_rust_fn`,
`set_global_named_rust_fn`) persists for the State's life, reachable or not."

## DOC-005 - The docs name the wrong error for over-wide dynamic calls

Scope: `vm-dispatch`.

`docs/limits.md` ("Runtime call width") says "`f(table.unpack(t))` or `f(...)`
with more values raises a too-many-arguments error". The `MAX_ARGUMENTS` doc in
`src/limits.rs` says the same, and the `MAX_RESULTS` doc says the same of
`return ...`. What the code does:

- `table.unpack`/`unpack` refuses a span of 256 or more itself, with
  `TooManyResults` (`lua_std.rs` `unpack_values`), before `call_window` sees a
  count. So `f(table.unpack(t))` raises too-many-results.
- `f(...)` can never exceed the cap: a function's varargs are capped by the
  argument cap at its own entry (`call_window`, `u8::try_from`), so `...` holds
  at most 255 values. The same holds for `return ...`.
- `TooManyArguments` actually comes from a fixed prefix plus an open tail
  (`f(x, ...)`, `f(x, table.unpack(t))`; `call_window`, `ArgCount::Dynamic`) and
  from a `__call` receiver pushed past 255 (`begin_callable_call`).

A script author following the docs looks for the wrong error kind.

## DOC-006 - The Frame invariant overstates the stack length

Scope: `vm-dispatch`.

The doc comment on `struct Frame` (`frame.rs`): "While its bytecode runs the
stack is at least `top` long; it is exactly `top` except between an instruction
that leaves open results and the one that consumes them...". An open producer
can leave the stack shorter than `top`: `reg_vararg` with `RetCount::All`
truncates to `dst` and appends however many varargs exist, possibly none; an
`OP_CALL` with `RetCount::All` leaves `func_slot + count`. The verifier's
open-results protocol (`produces_open`/`consumes_open`, `open_window_fits`)
makes it safe because only a consumer can run next, but the unconditional "at
least `top`" is false. A future handler that pushes at `stack.len()`
(`instr_tfor_call` and `resolve_index` push there) would land inside the frame's
registers if it ever ran between a producer and its consumer. Reword to "exactly
`top`, except between an open producer and its consumer, when it may be shorter
or longer".

## DOC-007 - Stale stack-VM vocabulary in eval.rs comments

Scope: `vm-dispatch`. Cosmetic. (The same leftover in
`reference/error-reporting.md` is part of DOC-002.)

- `eval.rs` `call_unguarded`: "ArgCount::Dynamic is resolved by the executing
  Lua Frame against its own pending base. Hosts never own such a base". The
  register VM has no pending base; `call_window` derives the count from
  `stack.len()`.
- `eval.rs` `invoke_rust_fn` doc: "Every RustFn call - `call_op` and the
  generic-for fast path alike". No `call_op` exists; the callers are
  `begin_call`, `begin_callable_call` and `instr_tfor_call_rust_fn`.

## DOC-008 - docs/budget.md under-describes the string library's charge units

Scope: `stdlib-strings`. No cost change needed for the doc fix
(`COST_MODEL_VERSION` untouched). Related deliberate double charges (`%s`/`%q`
in format, pattern bytes in find/match/gsub/gmatch) are already recorded in
`notes/todo.md` "Known laterals"; the template charge below is not among them.

Claim broken: `docs/budget.md` "Native charges / String library" lists the units
each site measures ("A charge site measures its work in the units listed
above"), and "Flat charges are deliberate" makes those units part of the cost
model. The code charges units the page does not list:

- Returned capture bytes: `find` (Lua-pattern path), `match` and every `gmatch`
  iteration charge the byte length of the captures they push
  (`preflight_capture_bytes`). `gsub` charges the capture bytes it passes to a
  replacement function, or uses as the table key.
- `gsub` output: unmatched subject segments copied to the result are charged per
  byte (`append_bytes` for `s[pos..start]` and the tail), and so is the whole
  match when a function or table replacement keeps the original. The page says
  only "gsub also charges replacement text bytes".
- The template is charged twice per application: each application of a string
  replacement charges `repl.len()` up front (`ReplTemplate::repl_len`, and the
  first application's `state.charge(.., repl.len())`), then charges every
  literal byte and capture byte it emits. A literal template `"xyz"` costs 6 per
  match, not 3. The comment at the top of `string.rs` names the pattern-bytes
  double charge but not this one.
- `format`'s `%s` charges the rendering of any argument, numbers and
  `__tostring` results included, before precision truncation. The page says
  "bytes of string arguments consumed".

The hunter adds a design question: a literal template is charged twice while a
function replacement returning the same text is charged once. If that asymmetry
is unintended, removing it is a `COST_MODEL_VERSION` bump.

## DOC-009 - interrupt.rs files the empty-pattern gsub under the wrong safepoint class

Scope: `stdlib-strings`. Doc nit; the poll itself is correct.

`src/vm/interrupt.rs` lists the empty-pattern `string.gsub` among "the library
loops whose trip count a script chooses by a number". Its trip count is the
subject length plus one, not a numeric argument. The poll in
`gsub_empty_pattern` is right. Reword, for example "each position of the
empty-pattern `string.gsub`". `docs/budget.md` lists it neutrally and is fine.

## DOC-010 - open_libs comments claim more atomicity than the code has

Scope: `stdlib-core`. Wording.

The `STDLIB_SETUP_HEADROOM` doc in `src/lua_std.rs` says the up-front stack
reservation "keeps the operation all-or-nothing". Under `snapshot`,
`rust_fns.bind_catalog()` runs before `open_base`, and the base, math, string
and table installs can still fail after the preflight (table growth, global
insertion and string interning are fallible). If one does, the catalog is bound
and some libraries are installed. Only an allocation failure gets there in
practice. Fix: "all-or-nothing with respect to the stack cap and registry
conflicts".

## DOC-011 - MAX_SOURCE_NAME_BYTES is documented as UTF-8 bytes

Scope: `cost-cli`.

`limits::MAX_SOURCE_NAME_BYTES` is documented as "Maximum UTF-8 source-name
bytes", but chunk names are arbitrary bytes (`Program::compile` docs), and the
CLI passes non-UTF-8 paths verbatim.

## DOC-012 - Lateral: save-state.md does not warn that anchored-only values are lost on save

Scope: `vm-state`. By design (README says anchors are not saved); a doc
suggestion.

`rest_state` binds `registry: _` as "allowed in a snapshot, not saved", and the
snapshot `debug_assert` cross-check in `save_state` cannot catch an
anchored-only value missing from a save. An embedder who anchors their only
reference to game state loses it on save with no diagnostic beyond
`SaveDiagnostics::anchor_count`. Worth a line in `docs/save-state.md` next to
"Not saved: ... anchors".

## DOC-013 - Lateral: basic.rs module doc calls itself the standard library

Scope: `stdlib-core`. Nit.

The module doc of `src/lua_std/basic.rs` reads "Lua's Standard Library"; the
module is the base library.
