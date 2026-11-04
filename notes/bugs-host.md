# Host-boundary findings: State API, error rendering, cost analysis, CLI

Filed from the 2026-11-04 defect hunt (scopes `vm-state` and `cost-cli`, with
one entry also from `stdlib-core`). Hunt reports are work documents and may be
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

## HOST-001 - set_global_named_rust_fn inside with_restricted_env keeps the registration but loses the global

Scope: `vm-state`. Hunter's label: low-medium.

Claims: `with_restricted_env`'s doc says "whatever `f` stores in it, raw keys
included, is discarded with it". `open_libs` is refused inside `f`
(`ErrorKind::OpenLibsInRestrictedEnv`) because "the libraries would land in the
temporary environment, which is discarded on restore, while the shared string
metatable and the snapshot token map they also replace outlive it".
`set_global_named_rust_fn` promises to "install it as global `name`" and "A
rejected call changes nothing".

`set_global_named_rust_fn` has the same split and is not refused. Inside `f` it
registers the id in `rust_fns` (which outlives the environment) and writes the
global into the restricted table (which `resume` discards). After `f` returns
the global is gone but the id is still bound, so the obvious retry, calling it
again outside, fails with `DuplicateFunctionRegistration`. The call is accepted
and half its effect is then silently undone.

Fix the hunter gave: refuse `set_global_named_rust_fn` inside a restricted
environment for the same reason as `open_libs` (generalising the kind to
something like `SetupInRestrictedEnv { api }`), or document the split on both
methods. Plain `register_rust_fn`/`add_rust_fn` are fine inside `f`: they
install nothing into the environment.

## HOST-002 - The anchor release queue is a second host input read mid-run

Scope: `vm-state`. Replay-visible through `%p`/`tostring` identity.

Claim broken: `docs/determinism.md`: "The library reads no host state ... The
one host input polled during a run is the `Interrupt` flag."

`collect_garbage` starts with `self.registry.drain_released()`, reading the
`Arc<Mutex<Vec<AnchorKey>>>` that `Anchor::drop` writes from any thread.
Automatic collections run mid-run at allocation points. If a host thread drops
an `Anchor` while a script runs, timing decides whether the drop lands before
collection N or N+1, and so whether a value only that anchor held is freed at N.
That decides `object_count()` at every later observation, and, by
determinism.md's own rule ("a string that is dropped and later rebuilt with the
same bytes keeps its id only if no collection ran in between"), the `%p` /
`tostring` identity a rebuilt string shows. A drop on the State's own thread or
inside a callback is deterministic; only a cross-thread drop concurrent with a
run is affected, which `Anchor`'s `Send + Sync` and the module doc ("a handle
can be dropped on any thread") explicitly invite.

Fix options the hunter gave: (a) drain only at host-boundary points (the
prologue of `guarded` entries, `Registry::insert`, explicit `gc_collect`), never
in an automatic collection, so a run sees a fixed anchor set; no hot-path cost.
Or (b) name the queue in determinism.md as a second host input, with the rule
"drop anchors on the State's thread or between runs".

## HOST-003 - Unnamed chunks and location-less errors render misleading prefixes

Scope: `vm-state`. Misleading messages; no determinism or cost effect.

In `Error`'s and `StackFrame`'s `Display` (`src/error.rs`):

- A traceback frame of an unnamed Lua chunk renders as `[C]:3: in main chunk`.
  `[C]` is reference Lua's marker for a C function; here it never means a native
  frame, since Rust callbacks are never traceback frames
  (`reference/error-reporting.md`). `Program::compile` says an unnamed chunk's
  "locations render without a name".
- The prefix for a runtime error in an unnamed chunk is `3:0: msg`. Runtime
  errors never have a column, so `:0` is noise, and the named-chunk arm
  (`a.lua:3: msg`) omits the column: the two disagree.
- An error with no location at all, which covers every host-misuse error
  (`InvalidStackIndex`, `InvalidAnchor`, `AnchorNil`, `CostingOff`,
  `StatePoisoned`, `DynamicArgumentsUnsupported`, ...), renders as
  `0:0: invalid stack index (-1)`. `Error::without_location` is documented as
  "without source-location information", but `Display` presents one.

Fix the hunter gave: render an unnamed frame as `?:3:` (or `[string]:3:`), omit
`:column` when the column is 0, and print no prefix when both line and source
are absent. The one test pinning `0:0:`
(`tests/integration/failures/error_prefix_selection.rs`, the unknown-caller
case) covers a selected caller with line 0, which would become "no prefix" under
that rule, consistent with the existing `TraceFrame` past-the-end rule.

## HOST-004 - Latent panic: pattern-cache eviction relies on an uncoupled tuning constant

Scope: `vm-state`. Smell; unreachable today.

`State::cached_pattern_at` evicts with `while len >= ENTRIES || bytes +
pattern_len > PATTERN_CACHE_BYTES { shift_remove_index(0).expect("a nonempty
pattern cache has an oldest entry") }`. If one pattern is longer than
`PATTERN_CACHE_BYTES`, the loop empties the cache and then panics on the empty
map. Today `tuning::PATTERN_CACHE_BYTES = MAX_STRING_BYTES` and every string is
capped there, but nothing ties the two; lowering the constant, which its doc
presents as free tuning, would turn a long pattern into a panic that poisons the
State. Fix: a `const { assert!(PATTERN_CACHE_BYTES >= MAX_STRING_BYTES) }`, or
better, skip caching (return the compiled program) when `pattern_len >
PATTERN_CACHE_BYTES`, which removes the invariant.

## HOST-005 - The cost-analysis report hides operation counts depending on the model

Raised by both `vm-state` and `cost-cli`.

Claim broken: `Program::analyze_cost_with` and `docs/budget.md` ("Static
analysis"): "Operation counts do not depend on the model; costs do."

`ScopeCost::fmt_indent` (`src/lib.rs`) decides from cost whether to print a
scope's category breakdown. With `own_cost == 0 && nested.is_empty() &&
function_calls() == 0` it prints `name: cost 0 (static)` and no counts;
otherwise it prints counts only when `own_cost > 0 || function_calls() > 0`.
Under `analyze_cost_with(&CostModel::zero())`, or any model weighting a scope's
charges at 0 (a design that makes arithmetic free, say), a scope full of
arithmetic and table writes renders as `cost 0 (static)` with no breakdown,
while the summary section above still prints `Arithmetic ...: N ops` for the
same operations. Whether counts appear also depends on whether the scope
contains a call: one `OP_CALL` makes every zero-weighted category appear. The
accessors are right; the `Display` is wrong.

Fix: gate the breakdown, and the `(static)` short form, on any category count
being non-zero, not on `own_cost`.

## HOST-006 - ScopeCost field and accessor docs say something other than what they hold

Scope: `cost-cli`.

- `instructions` is documented as "Total instruction count" but counts only this
  scope's own instructions (`analyze_chunk` counts `chunk.code`). Its sibling
  `total_cost` uses "total" to mean "including nested", so the word misleads.
  The six category accessors are own-only too, and nothing says so.
- `array_elements()`: "Number of array elements initialized". An open `SET_LIST`
  counts 0 even when the constructor certainly stores elements: `{1, 2, f()}`
  reports 0 although 1 and 2 are always stored. The budget doc covers the open
  count; the accessor doc contradicts it.
- `table_writes()`: "table field writes (`t.x = v`, `t[k] = v`)". It also counts
  constructor keyed fields (`INIT_FIELD`, `INIT_FIELD_PINNED`, `INIT_INDEX`, all
  of class `TableWrite`). The report label `Table writes (t[k]=v)` has the same
  gap.

## HOST-007 - A failed stdout write during print exits 1 without the cost summary

Scope: `cost-cli`.

Claim at stake: `docs/cli.md`, "the cost summary is still attempted before an
execution error is reported".

`CliCallbacks::on_print` calls `write_stdout_line_or_exit`. A non-EPIPE write
error (ENOSPC, EIO, a full redirected file) goes through `exit_for_stdout_error`
to `report_error`, which calls `exit(1)` from inside the running callback. No
`Cost used:` line is printed, and the script's own error, if any, is never
reached. Whether that is an "execution error" is a matter of wording, but it is
a run that started and failed with exit 1, and the ordering promise does not
hold. Fix options: document that a stdout failure ends the run immediately, or
latch the failure in the callbacks and have `main` report it after the summary.

## HOST-008 - The CLI does not provision the native-stack envelope it relies on

Scope: `cost-cli`. Hunter's label: low to medium.

Claims at stake: `docs/limits.md` "Native stack / Envelope" (2 MiB release,
4 MiB dev) and its embedder advice that "the main thread usually has 8 MiB
already". `docs/cli.md` documents Windows behaviour (WTF-8 chunk names), so
Windows is a target the CLI speaks for.

`main` compiles and runs the script on the process main thread. On Windows the
main thread's default stack is 1 MiB, below both envelope figures. A script
nesting native calls to `MAX_NATIVE_CALL_DEPTH` (sort comparators, metamethod
handlers, iterators), or source at the syntax budget, can overflow and abort the
process, where the documented outcome is a clean limit error. `docs/cli.md` does
allow for "process aborts", so this is not strictly a broken CLI promise, but
"usually 8 MiB" is Linux/macOS-specific and the repo's own embedder is the one
host guaranteed to hit it. Fix: run compile plus call on a
`std::thread::Builder` thread with an explicit stack size (for example 8 MiB),
and drop the "usually" sentence.

## HOST-009 - The CLI labels every compile failure "parsing source:"

Scope: `cost-cli`.

`main` wraps every `Program::compile` error as `parsing source: {error}`, and
`docs/cli.md` calls them all "parse errors". Errors that are not parse errors
get the same label: `SourceSizeExceeded`, an over-long chunk name (the path),
`UnsupportedFloatEnvironment`, and compiler internal faults and
self-verification failures (reported as compile errors by design). The
underlying text names the real cause, so the effect is a misleading prefix, not
lost information. `compiling:` or no prefix would be accurate.

## HOST-010 - CLI rendering nits in the analysis output

Scope: `cost-cli`.

- `--analyze` writes `format!("{}\n", analysis)`, but `CostAnalysis`'s `Display`
  already ends every line with `\n`, so the report ends in a blank line.
  `--dump-bytecode` writes `to_string()` and does not.
- `(static)` appears only on zero-cost leaf scopes, though every number in the
  report is static: a leftover of the old "(free)" fix (see the tripwire test).

## HOST-011 - The CLI reads unbounded input before rejecting it

Scope: `cost-cli`. Robustness.

`Input::read` calls `read_to_end` or `fs::read` with no cap; only then does
`Program::compile` reject anything over `MAX_SOURCE_BYTES`. A huge file or an
endless stdin is buffered in full first, or exhausts memory. Reading at most
`MAX_SOURCE_BYTES + 1` bytes would fail fast with the same error.

## HOST-012 - Lateral: the on_error contract does not say which other host APIs are unreported

Scope: `vm-state`. Probably intended; the contract is silent.

`HostCallbacks::on_error` lists the APIs whose errors are reported (`call`,
`call_anchor`, `get_table`, `set_table`, `tostring_bytes`,
`to_string_with_meta`) and the misuse that is not. `State::concat` (type errors,
`StringSizeExceeded`), `set_metatable_of`, `table_insert_at`/`table_remove_at`
and `raw_field` fail with execution-like kinds, are in neither list, and are not
reported. One sentence would settle it ("other host API failures are returned
only").

## HOST-013 - Lateral: host_print and current_source do not skip the gmatch shim

Scope: `vm-state`. Unreachable today.

`host_error` and `build_stack_trace` skip the gmatch wrapper frame;
`host_print` and `current_source` do not. Unreachable because the shim only
calls the private iterator, which runs no host code or `print`. If the shim ever
calls user or host code, `print` locations would name `string.gmatch`.

## HOST-014 - Lateral: every State allocates names for the whole function catalog

Scope: `vm-state`. Performance.

`RustFnTable::new` allocates one `Box<str>` name per catalog entry for every
State. Under `snapshot`, `bind_catalog` allocates two more per entry (`ids` and
`by_id`), and `catalog_conflict` formats every id again on each `open_libs`. All
are compile-time strings. A `Cow<'static, str>` (or a static-name side table for
catalog indices) removes the per-State work, which matters to hosts building one
State per script or per tick.

## HOST-015 - Lateral: whether a public method is guarded is a convention, not a type

Scope: `vm-state`. Structural.

Whether a public method needs `guarded` (because it can run Lua) or only
`check_usable` is decided by hand and checked by `scripts/poison_contract.py`
plus the native-calls inventory. The hunter traces the stale guard list in
`reference/error-reporting.md` (filed in `notes/bugs-docs.md`) and HOST-001 to
that hand-maintained split drifting.
Structural option: route every Lua-reaching public method through one
`HostEntry` wrapper, or make `call_unguarded` and kin take a zero-sized
`Reentry` token that only `guarded` and `invoke_rust_fn` can mint. "Public entry
that can run Lua but is not guarded" then becomes a compile error, and the
reference docs can point at the type instead of listing methods.

## HOST-016 - Lateral: the static cost analysis lives in the crate root and counts per category, not per charge

Scope: `cost-cli`. Structural.

- `ScopeCost`, `CostAnalysis`, `ScopeTotals`, `write_summary_categories` and two
  `Display` impls are about 300 lines in `src/lib.rs`, the file that otherwise
  holds the API surface. They belong in their own module (for example
  `src/cost_analysis.rs`, beside `cost_model.rs`), with `OPCODE_COST_LABELS` and
  `CostSummarySection` moved out of `instr.rs` with them; presentation tables
  currently live in the instruction-encoding module.
- The analysis keeps a second classification (`OpcodeCost`: Arithmetic,
  Negation, TableCreation, TableWrite, ArrayElements, Call, Free) parallel to the
  `Charge` registry and prices it through the registry. Because the categories
  merge `+ - * / % ^`, a host analysing under a model with `Pow` at 10 cannot see
  where its cost comes from. Counting units per `Charge` would make counts
  model-independent by construction (HOST-005 disappears), make cost a dot
  product with any `CostModel` (so `analyze_cost_with` prices one analysis
  instead of re-walking), and shrink `OpcodeCost` to what dispatch needs (`Call`
  survives only as a statistic).
- `CostAnalysis` does not record which model priced it; its `Display` prints
  "Static cost estimate" with no model, so reports from two models are
  indistinguishable.
