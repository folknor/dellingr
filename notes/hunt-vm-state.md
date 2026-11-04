# Hunt: vm-state

Scope: `src/vm.rs`, `src/error.rs`, `src/vm/inventory.rs`, `src/vm/anchor.rs`,
`src/vm/rust_fn.rs`, `src/host.rs`, `src/lib.rs`, `src/vm_aux.rs`,
`src/native_call_trace.rs`, plus the native re-entry bodies they own in
`src/vm/eval.rs` (`call_unguarded`, `begin_call`, `invoke_rust_fn`,
`run_invocation`, `unwind_invocation`, `fail_call`, `report_escaping`,
`locate_error`).

The core boundary machinery held up under tracing: every error arm of
`call_unguarded` (preflight, native depth, admission refusal, `__call` chain,
Rust callee error, `ResultCountExceedsStack`, result padding) restores
`stack_bottom`, depth counters and chain units and truncates exactly to the
callee slot; `call_anchor` removes its inserted callee on every post-insert
failure; poison and `PoisonUnwind` translation behave as documented at every
nesting I could construct (including `with_restricted_env` inside a callback);
`for_each_gc_root` and `rest_state` classify every field correctly. Most of
what follows is documentation that no longer matches the code, plus a few
smaller behavioural gaps.

---

## 1. Three public docs still describe the removed name lookup through globals (medium, doc vs code)

**Claim broken:** `reference/error-reporting.md` ("Argument error names") and
the code (`State::active_rust_function_name`, `vm/rust_fn.rs`) say an
argument error's function name is the entry's own diagnostic name, fixed when
the entry was created: "Nothing is discovered at error time. The name does not
depend on which globals exist ... or on the `snapshot` feature." Commit
912c2d5 made that change. Three host-facing docs still describe the old
mechanism:

- `src/error.rs`, `ArgError` doc comment: "the VM looks the callback up only
  then: first the first current global bound to the same function pointer,
  then, with the `snapshot` feature, the callback's registered stable id.
  Callbacks found by neither remain anonymous. The name ... can differ between
  builds."
- `src/host.rs`, `HostCallbacks::on_error` doc: "Generic Rust callback checks
  can resolve a label from current globals or, with `snapshot`, a registered
  function id; aliases and build settings can affect the result."
- `docs/determinism.md`, "Error labels": "get a label from the current global
  bindings or, with the `snapshot` feature, from the registered function id.
  The label can therefore differ between feature sets or builds, and when
  globals are rebound."

All three are wrong today. Function pointers are never compared, the name
does not depend on globals or features, and a host function is never
anonymous while it runs. The rustdoc on `ArgError` and `on_error` is the first
place a host author looks.

**Fix:** reword all three to match the reference: the name is the entry's
diagnostic name (from `add_rust_fn` / `register_rust_fn` /
`set_global_named_rust_fn`, or the catalog id), fixed at creation, and it is
diagnostic text, not an identifier. `docs/determinism.md` can drop the
"differ between feature sets or builds" bullet entirely. The only remaining
cross-build variation is a host renaming its own entries.

## 2. `reference/error-reporting.md` "Panics and poisoning" lists a guard that does not exist and misses one that does (low, binding reference doc vs code)

- It says marking happens "in the unwind guards of `with_rooted_values`,
  `with_restricted_env` and the vararg frame". No vararg-frame guard exists.
  The only `catch_unwind` sites that poison are `invoke_rust_fn`,
  `with_rooted_values`, `with_restricted_env` and `State::guarded`. Varargs
  now live in `InFlight::vararg_stack` and have no guard of their own.
- Its list of guarded entry points ("`call`, `call_anchor`, `get_table`,
  `to_string_with_meta`, `tostring_bytes`, `with_restricted_env`") leaves out
  `set_table`, which is guarded (`table_ops.rs`) and was added in 0a76dbc.
- "the call stack, stack bottom, pending bases, transient roots and open
  upvalues": "pending bases" names no current State field.

`reference/` is the binding tier, so all three are findings. **Fix:** list
the four real guard sites, add `set_table` (or, more durably, say "every
public entry point that can execute Lua; `scripts/poison_contract.py`
enforces the prologue" rather than enumerating), and drop "pending bases".

## 3. `State::is_poisoned` says the cost budget stays usable; `set_cost_budget` refuses (low, doc vs code)

`is_poisoned` doc: "Infallible host controls and observations, such as the
cost budget, user data, counters and this method, remain available."
`set_cost_budget` is fallible and starts with `check_usable()`, so on a
poisoned State it returns `StatePoisoned`. Its own doc says so ("the usual
refusal of a poisoned State"). Only the observations `cost_used` and
`cost_remaining` stay available. **Fix:** say "cost observations", or list
the real survivors: `cost_used`, `cost_remaining`, user data, GC controls,
`seal_globals`, `set_interrupt`, `release_anchor`, counters.

## 4. `docs/limits.md` limits the never-reclaimed Rust function entries to `snapshot` registrations (low, doc vs code)

`docs/limits.md`, "Never reclaimed": "with the `snapshot` feature, Rust
function registrations persist while unreachable, one per registered id."
The function table (`RustFnTable`) is append-only in every build. Every
`add_rust_fn` call, which needs no feature and takes no id, adds an entry
that lives as long as the State. The doc comments on `State::add_rust_fn` and
`RustFnHandle` say so ("Entries live as long as the State ... creating one
per call grows the State's function table for its whole life"). The limits
page is the one place a host would look for unbounded growth, and it states a
narrower case. **Fix:** "Never reclaimed: every Rust function entry
(`add_rust_fn`, `register_rust_fn`, `set_global_named_rust_fn`) persists for
the State's life, reachable or not."

## 5. `set_global_named_rust_fn` inside `with_restricted_env` keeps the registration but loses the global (low-medium, contract inconsistency)

`with_restricted_env` doc: "whatever `f` stores in it, raw keys included, is
discarded with it." `open_libs` is refused inside `f`
(`ErrorKind::OpenLibsInRestrictedEnv`) because "the libraries would land in
the temporary environment, which is discarded on restore, while the shared
string metatable and the snapshot token map they also replace outlive it".

`set_global_named_rust_fn` has the same split and is not refused. Inside `f`
it registers the id in `rust_fns` (which outlives the environment) and writes
the global into the restricted table (which `resume` discards). After `f`
returns, the global is gone but the id stays bound, so the obvious retry,
calling `set_global_named_rust_fn(name, id, f)` again outside, fails with
`DuplicateFunctionRegistration`. The doc promises "install it as global
`name`" and "A rejected call changes nothing". This call is accepted, and half
of its effect is then silently undone.

**Fix:** refuse `set_global_named_rust_fn` inside a restricted environment
with the same reason `open_libs` has (generalise the kind to something like
`SetupInRestrictedEnv { api }`), or document the split on both methods. Plain
`register_rust_fn` / `add_rust_fn` are fine inside `f`: they install nothing
into the environment.

## 6. Unnamed chunks and errors with no location render misleading prefixes (low, misleading message)

`Error`'s `Display` and `StackFrame`'s `Display` (`src/error.rs`):

- A traceback frame of an **unnamed Lua chunk** renders as `[C]:3: in main
  chunk`. `[C]` is reference Lua's marker for a C function. Here it never
  means a native frame, because Rust callbacks are never traceback frames
  (`reference/error-reporting.md`). `Program::compile` says an unnamed
  chunk's "locations render without a name".
- The error prefix for a runtime error in an unnamed chunk is `3:0: msg`.
  Runtime errors never have a column, so `:0` is noise. The named-chunk arm
  (`a.lua:3: msg`) omits the column, so the two disagree.
- An error with **no location at all**, which covers every host-misuse error
  (`InvalidStackIndex`, `InvalidAnchor`, `AnchorNil`, `CostingOff`,
  `StatePoisoned`, `DynamicArgumentsUnsupported`, ...), renders as
  `0:0: invalid stack index (-1)`. `Error::without_location` is documented as
  "without source-location information", but `Display` presents one.

None of this touches determinism or cost. The AGENTS bar for an
error-message defect is "a misleading message", and `[C]` plus a fabricated
`0:0:` location qualify. **Fix:** render an unnamed frame as `?:3:` (or
`[string]:3:`), omit `:column` when the column is 0, and print no prefix when
both line and source are absent. The one test that pins `0:0:`
(`tests/integration/failures/error_prefix_selection.rs`, the unknown-caller
case) covers a selected caller with line 0, which would become "no prefix"
under that rule. That also fits the existing `TraceFrame` past-the-end rule.

## 7. The anchor release queue is a second host input read mid-run (low, contract `docs/determinism.md`)

`docs/determinism.md`: "The library reads no host state ... The one host input
polled during a run is the `Interrupt` flag."

`collect_garbage` begins with `self.registry.drain_released()`, which reads
the `Arc<Mutex<Vec<AnchorKey>>>` that `Anchor::drop` writes from any thread.
Automatic collections run mid-run, at allocation points. If a host thread
drops an `Anchor` while a script runs, the timing decides whether the drop
lands before collection N or N+1, and so whether a value only that anchor
held is freed at N. That decides `object_count()` at any later observation,
and, per determinism.md's own rule ("a string that is dropped and later
rebuilt with the same bytes keeps its id only if no collection ran in
between"), the `%p` / `tostring` identity a rebuilt string shows. That is
replay-visible output. A drop on the State's own thread, or from inside a
callback, is deterministic. Only a cross-thread drop concurrent with a run
is affected, which `Anchor`'s `Send + Sync` and the module doc ("a handle can
be dropped on any thread") explicitly invite.

**Fix (pick one):** (a) drain only at host-boundary points (the prologue of
`guarded` entries, `Registry::insert`, explicit `gc_collect`), never inside an
automatic collection, so a run sees a fixed anchor set; or (b) name the
queue in determinism.md as a second host input, with the rule "drop anchors
on the State's thread or between runs". (a) costs nothing on the hot path.

## 8. Latent panic: the pattern-cache eviction loop relies on an uncoupled tuning constant (smell)

`State::cached_pattern_at` evicts with
`while len >= ENTRIES || bytes + pattern_len > PATTERN_CACHE_BYTES { shift_remove_index(0).expect("a nonempty pattern cache has an oldest entry") }`.
If one pattern's length exceeds `PATTERN_CACHE_BYTES`, the loop empties the
cache and then panics on the empty map. Today that cannot happen because
`tuning::PATTERN_CACHE_BYTES = MAX_STRING_BYTES` and every string is capped
there, but nothing ties the two. Lowering the tuning constant, which its doc
presents as free implementation tuning, would turn a long pattern into a panic
that poisons the State. **Fix:** a `const { assert!(PATTERN_CACHE_BYTES >=
MAX_STRING_BYTES) }`, or, better, skip caching (just return the compiled
program) when `pattern_len > PATTERN_CACHE_BYTES`, which removes the
invariant.

## 9. `CostAnalysis` Display hides operation counts under a zero-weight model (low, `analyze_cost_with` doc)

`Program::analyze_cost_with`: "Operation counts do not depend on the model;
costs do." The accessors honour that, but `ScopeCost::fmt_indent` prints the
per-category breakdown only when `own_cost > 0 || function_calls() > 0`, and
prints `cost 0 (static)` with no counts otherwise. Under a `CostModel` that
zeroes the weights a scope uses (a design that makes arithmetic free, say),
the rendered report drops those counts, which it shows under the default
model. Display-only. **Fix:** gate the breakdown on any nonzero count, not on
cost.

---

## Lateral notes (not defects against a claim)

- **Unlisted host APIs in the `on_error` contract.** `HostCallbacks::on_error`
  lists the APIs whose errors are reported (`call`, `call_anchor`,
  `get_table`, `set_table`, `tostring_bytes`, `to_string_with_meta`) and the
  misuse that is not. `State::concat` (type errors, `StringSizeExceeded`),
  `set_metatable_of`, `table_insert_at` / `table_remove_at` and `raw_field`
  fail with execution-like kinds, are in neither list, and are not reported.
  Probably intended (no Lua runs), but the contract does not say. One
  sentence would settle it ("other host API failures are returned only").
- **`host_print` and `current_source` do not skip the gmatch shim**, while
  `host_error` and `build_stack_trace` do. It is unreachable today because
  the shim only calls the private iterator, which runs no host code or
  `print`. If the shim ever grows a call into user or host code, `print`
  locations would name `string.gmatch`.
- **Per-State catalog allocation.** `RustFnTable::new` allocates one
  `Box<str>` name per catalog entry for every State, and under `snapshot`
  `bind_catalog` allocates two more per entry (`ids` and `by_id`), with
  `catalog_conflict` formatting every id again on each `open_libs`. All of
  these are compile-time strings. A `Cow<'static, str>` (or a static-name
  side table for catalog indices) removes that per-State work, which matters
  to hosts that build one State per script or per tick.
- **`RestState` docs vs `registry`.** `rest_state` binds `registry: _` as
  "allowed in a snapshot, not saved". That is correct, but the snapshot
  `debug_assert` cross-check in `save_state` cannot catch an anchored-only
  value silently missing from a save. That is by design (README says so). An
  embedder who anchors their only reference to game state loses it on save
  with no diagnostic beyond `SaveDiagnostics::anchor_count`. That is worth a
  line in `docs/save-state.md` next to "Not saved: ... anchors".
- **Architecture: the guard list is a convention, not a type.** Whether a
  public method needs `guarded` (because it can run Lua) or only
  `check_usable` is decided by hand and checked by `scripts/poison_contract.py`
  plus the native-calls inventory. Findings 2 and 5 both come from that
  hand-maintained split drifting. A structural option: route every
  Lua-reaching public method through one `HostEntry` wrapper (or make
  `call_unguarded` and kin take a zero-sized `Reentry` token that only
  `guarded` and `invoke_rust_fn` can mint). Then "public entry that can run
  Lua but is not guarded" becomes a compile error, and the reference docs can
  point at the type instead of listing methods.
