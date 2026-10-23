# Defect hunt: surface (public API root, analyzer, CLI, harness, scripts)

Scope: src/lib.rs, src/main.rs, examples/hotpath.rs, scripts/ (crmath_tables.py
excluded), checked against AGENTS.md "Commands" / "Hotpath benchmarks",
docs/cli.md, docs/budget.md, docs/limits.md, reference/native-calls.md and the
scripts' own headers. Read-only; nothing was built or run.

Summary: the surface mostly does what it claims. The analyzer's opcode
accounting matches dispatch exactly: `static_units` against `Frame::run_frame`'s
`add_cost!` sites, `SET_LIST` open/fixed counts, templates and calls. The CLI
matches docs/cli.md on argument parsing, streams and closed-pipe handling.
`State::load` is atomic on rejection: the stack is preflighted, and
`resolve_bytecode_runtime` checks literal sizes before it touches the string
pool. `native_calls.py` matches what native-calls.md says it checks. The real
gaps are below, in rough order of consequence.

---

## 1. Hotpath harness: `MAX_WARM_BLOCKS` is not an upper bound

Claim broken: the doc comment on `MAX_WARM_BLOCKS` in examples/hotpath.rs
says "Upper bound on `WARM_BLOCK` marker pairs per run". AGENTS.md says the same:
"batched `WARM_BLOCK` sub-spans bounded by `MAX_WARM_BLOCKS` in the harness
source".

The code:

```rust
let block_size = (iterations / MAX_WARM_BLOCKS).max(1);
...
while done < iterations {
    let n = block_size.min(iterations - done);
```

The block size uses floor division, so the block count is
`ceil(iterations / floor(iterations / 32))`, and that can be nearly twice the
bound:

- `--iterations 63`: block_size 1, so 63 blocks.
- `--iterations 100`: block_size 3, so 34 blocks.
- `--iterations 95`: block_size 2, so 48 blocks.

Any count from 33 up to 63 overshoots, and so do many larger non-multiples of
32. The default of 20 is under the bound, so it only shows once `--iterations` is
passed. The fix is `iterations.div_ceil(MAX_WARM_BLOCKS)` for the block size
(the last block is shorter), which gives at most 32 blocks. The
`WARM_BLOCK` collapsed row in `brokkr sidecar --durations` then also stops mixing
two block sizes in its min/max: today, with 100 iterations, 33 blocks of 3 calls
and one block of 1 call share a single min/max row.

## 2. `scripts/hotbench.sh` discards the harness's exit status

Claims broken:

- examples/hotpath.rs says "Exit status is 1 for a failed run and 2 for a
  usage error. The process status lets the harness caller distinguish a complete
  run from a failure."
- AGENTS.md says the harness "exits nonzero if the script does not define a
  global `_bench()` function".

hotbench.sh is one of the two documented callers of the harness. It runs
`"$HOTPATH_BIN" "$SCRIPT" 2>&1` without checking `$?`, then goes on to the
hyperfine pass. The script's exit status is hyperfine's. A parse error, a missing
`_bench`, a runtime error in the cold or warm phase, or a FIFO failure prints
one stderr line inside the harness section, then the wall-time table follows
and the script exits 0. Nothing downstream can tell that the KV half is missing
or partial. The run should stop (or at least exit nonzero) when the harness
fails.

## 3. The static-analysis caveat understates what it leaves out

Claims broken:

- `CostAnalysis::caveat()` says "It excludes data-dependent native work in the
  string, pattern, and table libraries".
- docs/budget.md "Static analysis" uses the same wording.
- docs/cli.md says the CLI prints this text as the `--analyze` warning.

The wording implies that native charges which are *not* data-dependent are
counted. They are not. `OP_CALL` is `OpcodeCost::Call`, whose `static_units`
is 0, and the analyzer never looks at the callee. So these all add 0 to
`analyze_cost`:

- every math library flat charge (budget.md: "each function charges a flat 1",
  `math.floor`, `math.random` and the rest);
- `table.insert`, `table.remove` and `table.pack` (flat 1 each);
- the up-front `max(1, ...)` minimums of the string functions.

A script whose cost is all `math.*` calls in a loop analyzes to cost 0. The
analyzer counts only opcode charges, so the caveat should say it excludes every
native library charge, flat or data-dependent. The unit test
`static_analysis_excludes_data_dependent_native_work` only uses `string.upper`,
so it pins the narrow wording and misses this. A script author reading the
warning will underestimate flat-charged library calls.

## 4. Stale "255 sentinel" documentation in the public `limits` module and `ArgCount`

Claims at issue (these are public doc comments, so the doc is what is wrong):

- src/limits.rs, the comment above `MAX_LOCALS`/`MAX_FIXED_ARGS`: "Operand
  value 255 is reserved for dynamic argument and result counts."
- The doc comments on `MAX_FIXED_ARGS` ("before the dynamic-count sentinel") and
  `MAX_FIXED_RESULTS` ("before the all-results sentinel").
- docs/limits.md: "The dynamic-count sentinel is reserved."
- src/instr.rs: "The fixed-count limits are exactly the counts below the shared
  255 sentinel of the runtime count types".
- `ArgCount`'s doc: "Only instruction encoding rejects the sentinel alias".
- The comment block under it: "runtime counts never pass through `to_u8`, which
  asserts against the alias".

Since the register bytecode went to 16-bit operands, the open count is
`OPEN_COUNT = 0xFFFF`. Operand value 255 is an ordinary fixed count, and
`RetCount::to_operand` encodes `Fixed(255)` as 255 for `RETURN`. No 255 sentinel
exists in either the encoding or the runtime types. There is also no `to_u8` on
`ArgCount`: the encoder is `to_operand`, which asserts
`n <= MAX_FIXED_ARGS`. The limits of 254 still hold, but for a different reason:
`ArgCount::Fixed` is a `u8`, and runtime `Fixed(255)` is kept for `__call`
adding a receiver to 254 arguments. The docs give a reason that is now false. A
host reading `limits` is told about a reserved operand that does not exist.

## 5. CLI: parse errors in run mode get a cost line, and the error prefix depends on the mode

Claim at issue: docs/cli.md says "The cost summary is still attempted before a
**runtime** error is reported."

In src/main.rs's run path, `compiled.and_then(load).and_then(call)` folds a
compile error into the same `result`. So a syntax error in run mode prints
`Cost used: 0` to stderr before `Error: ...`. Nothing ran, and no runtime
error happened.

The error text also differs by mode for the same failure:

- `--analyze` prints `Error: parsing source: <e>`.
- Run mode and `--dump-bytecode` print `Error: <e>`.

This is minor, but the doc describes the cost line as belonging to runtime
failures. To match the doc, report a compile error before the cost summary,
with the same wording in all three modes. The other option is to say in the doc
that compile errors also get a cost line.

## 6. CLI exit-status statement is wider than what main.rs controls

Claim at issue: docs/cli.md says "The CLI only ever exits 0 or 1 (never 124 ...)".
The comment above `report_error` and the comment in diff_test.sh repeat it.

main.rs makes every *handled* path exit 0 or 1. But `State::guarded`
re-raises any non-marker panic payload (`resume_unwind`), so an internal VM
panic exits 101. A native stack overflow or allocation failure aborts with a
signal. The property diff_test.sh actually needs, "never 124", still holds,
because none of these produce 124. As written, though, the sentence promises
something about unhandled failures that the CLI does not do. Narrow it to "the
CLI's own exit paths use only 0 and 1" (or "never 124").

## 7. scripts/notes_move.py silently drops a section whose ID appears twice

The header says "Fails before writing anything if a placeholder has no section,
so a typo never silently no-ops." That contract is met. But if two source files
both hold `## X-001 - ...`, both are cut out of their sources. In
`sections[m.group(1)] = ...`, the second overwrites the first, so the first
copy is in neither the destination nor its source after the rewrite. That is
silent data loss in a maintainer helper whose stated purpose is to move
sections safely. The fix is to refuse duplicate IDs across sources, as it
already refuses missing ones.

notes_drop.py and notes_move.py also treat a `## ` line inside a fenced code
block as a section heading. This is low risk for findings documents, but the
header's "A section runs from its `## ` heading" is literal.

---

## Lateral findings and smells (outside the strict defect bar)

- **`scripts/native_calls.py` always exits 0.** It prints problems and stops,
  and the brokkr `script_check` relies on `match = "last-line"` against `ok`. A
  direct `python3 scripts/native_calls.py && ...` in any other context reads a
  failing inventory as a pass. Exiting 1 whenever problems are printed costs
  nothing.
- **Hotpath harness chunk name is `@<path>`.** The harness compiles with
  `format!("@{}", path)`, the reference-Lua convention, but dellingr uses chunk
  names verbatim. So harness errors and `print` locations read
  `@bench/x.lua:12:`, while the CLI on the same file says `bench/x.lua:12:`. No
  cost or KV is affected.
- **Hotpath harness `warm_avg_cost` truncates.** It is
  `warm_total_cost / iterations` in integer division, and `cost_per_us` is
  derived from the truncated value. For a workload whose per-call cost
  varies (state that grows across calls), the fingerprint hides changes
  smaller than `iterations` units. Either emit the total or emit the
  average with three decimals, like `warm_avg_us`.
- **`scripts/frame_sizes.py --body` over-blanks.** `re.sub(r"\b[0-9a-f]{4,}\b",
  "ADDR", body)` also hits any whole word of four or more letters from a-f, such
  as the x87 mnemonic `fadd` or the same kind of word inside a demangled
  `<symbol>`. Two different instructions can then diff as equal. Matching only
  hex runs that contain a digit, or anchoring to objdump's address columns,
  would avoid this.
- **`scripts/ab_pairs.py` always runs A before B in every round.** That is
  interleaved, but the slot is fixed. Any effect of a cell's position (the
  thermal or cache state left by the previous cell) always lands on the same
  arm. Alternating the order in odd rounds (ABBA) removes that bias for free.
- **`scripts/lib.sh` has two identical wrappers:** `dellingr_bin` only
  forwards to `dellingr_bin_built`, and callers use both names. One can go.
- **src/lib.rs `#[doc(hidden)] mod patterns;`.** `doc(hidden)` on a private
  module does nothing, which suggests it was once public. Separately, `pub mod
  cli` puts CLI-only prefix strings into the library's public API, and
  `COST_MODEL_GOLDEN_FINGERPRINT` is a public `doc(hidden)` test constant. Both
  are pre-1.0 surface that only the binary and the tests need. They could live
  in main.rs and a `#[cfg(test)]` (or test-support) location.
- **diff_gate.sh's "debug vs release does not change the behavior under test
  (identical VM semantics, only speed)".** The per-process timeout is the
  fixed 5 s from lib.sh, so "only speed" can change the result: a heavy
  example that passes with the release binary can TIMEOUT with the debug one.
  The same holds for tests/diff_test.rs, which drives the gate with the dev
  binary of each check shape. Worth keeping in mind before trusting a TIMEOUT
  line from either.
- **The analyzer's per-scope "free" line** (`cost 0 (free)`) is printed for any
  scope with zero static cost and no calls. That includes a scope whose only
  costed work is an open `SET_LIST` (`return {...}`), which can charge any
  amount at runtime. This is consistent with the documented counting rule, but
  "free" is a stronger word than "static cost 0".
