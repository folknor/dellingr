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

## HOST-006 - The analysis report's table-writes label omits constructor fields

Scope: `cost-cli`. The `ScopeCost` accessor docs are fixed; this residue is
one report label.

The table-writes category also counts constructor keyed fields
(`INIT_FIELD`, `INIT_FIELD_PINNED`, `INIT_INDEX`, all of class `TableWrite`),
and the `table_writes()` doc now says so. The report label in `src/instr.rs`
(`OPCODE_COST_LABELS`) still reads `Table writes (t[k]=v)`. Reword it to cover
keyed constructor fields; tests that pin the analysis report text move with it.

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

## HOST-011 - The CLI reads unbounded input before rejecting it

Scope: `cost-cli`. Robustness.

`Input::read` calls `read_to_end` or `fs::read` with no cap; only then does
`Program::compile` reject anything over `MAX_SOURCE_BYTES`. A huge file or an
endless stdin is buffered in full first, or exhausts memory. Reading at most
`MAX_SOURCE_BYTES + 1` bytes would fail fast with the same error.

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
plus the native-calls inventory. The hunter traced a stale guard list in
`reference/error-reporting.md` (since corrected) and HOST-001 to that
hand-maintained split drifting. Structural option: route every Lua-reaching
public method through one `HostEntry` wrapper, or make `call_unguarded` and kin
take a zero-sized `Reentry` token that only `guarded` and `invoke_rust_fn` can
mint. "Public entry that can run Lua but is not guarded" then becomes a compile
error, and the reference docs can point at the type instead of listing methods.

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
