# Hunt: cost-cli (static cost analysis, bytecode listing, CLI)

Scope as given: `src/cost_model.rs`, `src/disassemble.rs`, `src/main.rs`.
Note first that the scope description is stale: `Program::analyze_cost`,
`ScopeCost`, `CostAnalysis` and all report rendering live in `src/lib.rs`, not in
`src/cost_model.rs`. `cost_model.rs` holds only the `Charge` registry, `CostModel`
and `Costing`. I reviewed the analysis in `lib.rs`, plus `instr.rs` (cost classes,
`static_cost`, `fixed_charge`, labels) and the dispatch loop in `vm/frame.rs`.

## What checks out

These were the agreement questions in the brief. I traced them and they hold:

- **Static vs dispatch charges agree per opcode.** Both sides use the same const
  tables. `Instr::static_cost` charges `fixed_charge(op)` at the model weight, and
  the dispatch arm's `fixed_cost!` charges the same `Instr::fixed_charge` at
  `state.cost.weight`. For ranges, a const assert makes every member share one
  charge. `SET_LIST` decodes its count the same way on both sides (`b() !=
  OPEN_COUNT` vs `set_list_count()`). An open count is 0 statically and the live
  count at runtime, as `docs/budget.md` says.
- **No other charges on opcode paths.** No opcode path in `src/vm/` charges
  anything besides these. The only `charge(` in `vm/` is `table.sort` in
  `table_ops.rs`, which is native.
- **The `vm/frame.rs` doc-table test exists** and checks every opcode class row,
  as `docs/budget.md` says.
- **Codegen does not duplicate code.** It does not copy loop conditions or other
  expressions (`while`, `repeat`, numeric and generic `for`, `if` in
  `codegen.rs`), so "loops and branches count once" holds bytecode-wise.
- **The `string.gmatch` wrapper chunk has no costed opcodes.** It runs Lua
  bytecode outside the analyzed program but adds no opcode charge.
- **Scope names are bounded.** Nested-function names are truncated to
  `MAX_CHUNK_NAME_BYTES` in `finalize_at_depth`, so the scope names in the
  analysis are bounded.
- **CLI `--limit` handling.** `set_cost_budget` accepts negative budgets, so the
  CLI's `.expect` on it cannot fire for any `i64` the parser accepts (the CLI
  State is metered and fresh).
- **CLI contract.** Flags, mutual exclusions, short forms, `-`/`--` handling,
  stderr/stdout routing, the EPIPE-to-0 policy, "no cost summary on parse
  error" and "cost summary before execution error" all match `docs/cli.md`.

## Findings

### 1. The per-scope report hides operation counts depending on the model (low)

Claim broken: `Program::analyze_cost_with` and `docs/budget.md` ("Static
analysis"): "Operation counts do not depend on the model; costs do."

`ScopeCost::fmt_indent` (`src/lib.rs`) decides whether to print a scope's
category breakdown from its cost:

- `own_cost == 0 && nested.is_empty() && function_calls() == 0`: prints
  `name: cost 0 (static)` and no counts.
- otherwise it prints counts only when `own_cost > 0 || function_calls() > 0`.

Under `analyze_cost_with(&CostModel::zero())`, or any model that weights a
scope's charges 0, a scope full of arithmetic and table writes renders as
`cost 0 (static)` with no breakdown. The summary section above it still prints
`Arithmetic ...: N ops` for the same operations. Whether counts appear also
depends on whether the scope happens to contain a call: one `OP_CALL` makes
every zero-weighted category appear. So the rendered counts depend on the model.
The struct is right; the `Display` is wrong.

Fix: gate the breakdown, and the "(static)" short form, on "any category count
is non-zero", not on `own_cost`.

### 2. `ScopeCost` field and accessor docs say something other than what they hold (low)

Claim broken: the doc comments on the public type.

- `instructions` is documented as "Total instruction count", but it counts only
  this scope's own instructions (`analyze_chunk` counts `chunk.code`). Its
  sibling `total_cost` uses "total" to mean "including nested", so the word is
  actively misleading. The six category accessors are own-only too, and nothing
  says so.
- `array_elements()`: "Number of array elements initialized". An open
  `SET_LIST` counts 0 even when the constructor certainly stores elements. For
  example, `{1, 2, f()}` reports 0 although 1 and 2 are always stored. The
  budget doc covers the open count; the accessor doc contradicts it.
- `table_writes()`: "table field writes (`t.x = v`, `t[k] = v`)". It also counts
  constructor keyed fields (`INIT_FIELD`, `INIT_FIELD_PINNED`, `INIT_INDEX`, all
  `TableWrite` class). The report label `Table writes (t[k]=v)` has the same gap.

### 3. A failed stdout write during `print` exits 1 without the cost summary (low)

Claim at stake: `docs/cli.md`, "the cost summary is still attempted before an
execution error is reported".

`CliCallbacks::on_print` calls `write_stdout_line_or_exit`. A non-EPIPE write
error (ENOSPC, EIO, a full redirected file) goes through `exit_for_stdout_error`
to `report_error`, which calls `exit(1)` from inside the running callback. No
`Cost used:` line is printed, and the script's own error, if any, is never
reached. Whether this counts as an "execution error" is a matter of wording. It
is a run that started and failed with exit 1, though, and the doc's ordering
promise does not hold for it.

Either document that a stdout failure ends the run immediately, or record the
error in the callbacks and finish through the normal path. For example, make
`on_print` latch the failure and have `main` report it after the summary.

### 4. The CLI does not provision the native-stack envelope it relies on (low to medium, lateral)

Claims at stake: `docs/limits.md` "Native stack / Envelope" (2 MiB release,
4 MiB dev), and its embedder advice "the main thread usually has 8 MiB already".
Also `docs/cli.md`, which documents Windows behaviour (WTF-8 chunk names), so
Windows is a target the CLI speaks for.

`main` compiles and runs the script on the process main thread. On Windows the
main thread's default stack is 1 MiB, below both envelope figures. A script that
nests native calls to `MAX_NATIVE_CALL_DEPTH` (sort comparators, metamethod
handlers, iterators), or source at the syntax budget, can overflow and abort the
process. The documented outcome is a clean limit error. `docs/cli.md` does allow
for "process aborts", so this is not strictly a broken CLI promise. The limits
doc's "usually 8 MiB" is Linux/macOS-specific, though, and the repo's own
embedder is the one host guaranteed to hit it.

Fix: run compile plus call on a `std::thread::Builder` thread with an explicit
stack size (for example 8 MiB), and drop the "usually" sentence.

### 5. Every compile failure is labelled `parsing source:` (low)

`main` wraps every `Program::compile` error as `parsing source: {error}`, and
`docs/cli.md` calls them all "parse errors". Errors that are not parse errors
get the same label:

- `SourceSizeExceeded`
- an over-long chunk name (the path)
- `UnsupportedFloatEnvironment`
- compiler internal faults and self-verification failures, reported as compile
  errors by design

The underlying error text names the real cause, so the effect is a misleading
prefix rather than lost information. If the prefix is meant to classify, it is
wrong. `compiling:` or no prefix would be accurate.

### 6. Rendering nits (low)

- `--analyze` writes `format!("{}\n", analysis)`, but `CostAnalysis`'s
  `Display` already ends every line with `\n`. The report therefore ends in a
  blank line. `--dump-bytecode` writes `to_string()` and does not.
- `(static)` appears only on zero-cost leaf scopes, although every number in the
  report is static. This is a leftover of the old "(free)" fix (see the tripwire
  test).

### 7. Stale doc in `limits.rs` (lateral)

`limits::MAX_SOURCE_NAME_BYTES` is documented as "Maximum UTF-8 source-name
bytes", but chunk names are arbitrary bytes (`Program::compile` docs, and the
CLI passes non-UTF-8 paths verbatim).

### 8. The CLI reads unbounded input before rejecting it (lateral, robustness)

`Input::read` calls `read_to_end` or `fs::read` with no cap. Only then does
`Program::compile` reject anything over `MAX_SOURCE_BYTES`. A huge file or an
endless stdin is buffered in full first, or exhausts memory. Reading at most
`MAX_SOURCE_BYTES + 1` bytes would fail fast with the same error.

## Structural opportunities

- **Move the analysis out of the crate root.** `ScopeCost`, `CostAnalysis`,
  `ScopeTotals`, `write_summary_categories` and two `Display` impls are about
  300 lines in `src/lib.rs`, the file that otherwise holds the API surface.
  They belong in their own module (for example `src/cost_analysis.rs`, beside
  `cost_model.rs`), with `OPCODE_COST_LABELS` and `CostSummarySection` moved out
  of `instr.rs` with them. Presentation tables currently live in the instruction
  encoding module.
- **Count per `Charge`, not per `OpcodeCost` category.** The analysis keeps a
  second classification (`OpcodeCost`: Arithmetic, Negation, TableCreation,
  TableWrite, ArrayElements, Call, Free) parallel to the `Charge` registry, and
  then prices it through the registry. Because the categories merge
  `+ - * / % ^`, a host analysing under a model with `Pow` at 10 cannot see where
  its cost comes from, and the report cannot show it.

  Counting units per `Charge` would make the counts model-independent by
  construction (finding 1 disappears). Cost becomes a dot product with any
  `CostModel`, so `analyze_cost_with` is a pricing step over one analysis, not a
  re-walk. `OpcodeCost` would shrink to what dispatch needs (`Call` survives
  only as a statistic).
- **Record the model in the analysis.** `CostAnalysis` does not say which model
  priced it. Its `Display` prints "Static cost estimate" with no model, so two
  reports from different models are indistinguishable.
