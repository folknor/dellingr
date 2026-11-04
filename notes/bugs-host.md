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
