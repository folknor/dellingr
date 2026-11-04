# Host-boundary findings: State API, cost analysis, CLI

Filed from the 2026-11-04 defect hunt (scopes `vm-state` and `cost-cli`). Hunt
reports are work documents and may be wrong; verify before acting.

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

## HOST-014 - Every State allocates names for the whole function catalog

Scope: `vm-state`. Performance; unmeasured, so any change goes through a
registered `brokkr dellingr` workload first.

`RustFnTable::new` allocates one `Box<str>` name per catalog entry for every
State. Under the `snapshot` feature, `bind_catalog` allocates two more per entry
(`ids` and `by_id`), and `catalog_conflict` formats every id again on each
`open_libs`. All of these names are compile-time strings. The cost is per State,
so it grows with how many States a host builds (one per script or per tick, for
example).

Approaches raised so far (not exhaustive): `Cow<'static, str>` names, or a
static-name side table for catalog indices.

## HOST-015 - Whether a public method may run Lua is a convention, not a type

Scope: `vm-state`. Structural.

Some public `State` methods can execute Lua (`call`, `call_anchor`, `get_table`
through `__index`, `set_table`, `tostring_bytes` and `to_string_with_meta`
through `__tostring`, `with_restricted_env`). These must enter through
`guarded`, which catches a panic, poisons the State and translates the error.
Methods that cannot run Lua only need `check_usable`, which refuses a poisoned
State. Which group a method belongs to is decided by hand. What keeps the
decision consistent is `scripts/poison_contract.py` and the native-calls
inventory (`reference/native-calls.toml`), not the type system.

The hunter traced two findings from the same hunt to this hand-maintained split:
a guard list in `reference/error-reporting.md` that named a guard that did not
exist and omitted `set_table` (since corrected), and `set_global_named_rust_fn`
being accepted inside a restricted environment (since refused).

Approaches raised so far (not exhaustive): route every Lua-reaching public
method through one `HostEntry` wrapper; or make `call_unguarded` and the other
re-entry functions take a zero-sized `Reentry` token that only `guarded` and
`invoke_rust_fn` can create, so a public method that can run Lua without being
guarded fails to compile and the reference docs can name the type instead of
listing methods.

Context noted during triage, not investigated: the re-entry functions are the
ones `vm/eval.rs` and `reference/native-calls.md` hold to native-frame-size
rules across `Frame::run`, so a change to their signatures meets those rules.

## HOST-016 - Static cost analysis: location, counting model and provenance

Scope: `cost-cli`. Structural.

Three separate observations about the static cost analysis:

- **Location.** `ScopeCost`, `CostAnalysis`, `ScopeTotals`,
  `write_summary_categories` and two `Display` impls are about 300 lines in
  `src/lib.rs`, the file that otherwise holds the public API surface. The
  report's presentation tables (`OPCODE_COST_LABELS`, `CostSummarySection`) live
  in `src/instr.rs`, the instruction-encoding module.
- **Counting model.** The analysis keeps its own classification (`OpcodeCost`:
  Arithmetic, Negation, TableCreation, TableWrite, ArrayElements, Call, Free),
  parallel to the `Charge` registry the cost model prices, and prices it
  through the registry. The categories merge `+ - * / % ^`, so a host analysing
  under a model where `Pow` costs more than the other arithmetic cannot see where
  its cost comes from. Whether operation counts depend on the model has already
  needed a hand fix in a fix wave (the per-scope report now decides from counts
  rather than cost).
- **Provenance.** `CostAnalysis` does not record which model priced it, and its
  `Display` prints "Static cost estimate" without naming the model, so reports
  from two models are indistinguishable.

Approaches raised so far (not exhaustive): a dedicated module for the analysis
(for example `src/cost_analysis.rs` beside `cost_model.rs`) with the
presentation tables moved out of `instr.rs`; counting units per `Charge`, which
would make counts model-independent by construction, make cost a weighted sum
over any `CostModel` so `analyze_cost_with` prices one analysis rather than
re-walking bytecode, and reduce `OpcodeCost` to what dispatch needs; recording
the model in the analysis and its report.

Context noted during triage: counting per `Charge` changes the shape of the
public analysis API and the report format; the move by itself does not.
