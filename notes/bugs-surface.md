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

# Surface defects: public API, analyzer, CLI, harness, scripts

Filed from the 2026-10-23 defect hunt at a22d9c0 (scope `surface`). The hunter
read the code statically and built or ran nothing. It found the analyzer's opcode
accounting matching dispatch exactly (`static_units` against `Frame::run_frame`'s
`add_cost!` sites, `SET_LIST` open and fixed counts, templates and calls), the CLI
matching docs/cli.md on argument parsing, streams and closed-pipe handling,
`State::load` atomic on rejection, and `native_calls.py` matching what
native-calls.md says it checks. The surface hunter's stale "255 sentinel" finding
is filed with the other reports of it as CMP-005 in notes/bugs-compiler.md.

## SRF-001 - Hotpath harness: `MAX_WARM_BLOCKS` is not an upper bound

**Claim broken.** The doc comment on `MAX_WARM_BLOCKS` in examples/hotpath.rs:
"Upper bound on `WARM_BLOCK` marker pairs per run". AGENTS.md says the same:
"batched `WARM_BLOCK` sub-spans bounded by `MAX_WARM_BLOCKS` in the harness
source".

**Mechanism.**

```rust
let block_size = (iterations / MAX_WARM_BLOCKS).max(1);
...
while done < iterations {
    let n = block_size.min(iterations - done);
```

The block size uses floor division, so the block count is
`ceil(iterations / floor(iterations / 32))`, which can be nearly twice the
bound: `--iterations 63` gives block size 1, so 63 blocks; `--iterations 100`
gives block size 3, so 34 blocks; `--iterations 95` gives block size 2, so 48
blocks. Any count from 33 to 63 overshoots, and so do many larger non-multiples
of 32. The default of 20 is under the bound, so it shows only once
`--iterations` is passed.

**Fix.** `iterations.div_ceil(MAX_WARM_BLOCKS)` for the block size (the last
block is shorter), which gives at most 32 blocks. The `WARM_BLOCK` collapsed row
in `brokkr sidecar --durations` then also stops mixing two block sizes in its
min and max: today, with 100 iterations, 33 blocks of 3 calls and one block of 1
call share a single min/max row.

## SRF-002 - `scripts/hotbench.sh` discards the harness's exit status

**Claims broken**

- examples/hotpath.rs: "Exit status is 1 for a failed run and 2 for a usage
  error. The process status lets the harness caller distinguish a complete run
  from a failure."
- AGENTS.md: the harness "exits nonzero if the script does not define a global
  `_bench()` function".

**Mechanism.** hotbench.sh, one of the two documented callers of the harness,
runs `"$HOTPATH_BIN" "$SCRIPT" 2>&1` without checking `$?`, then goes on to the
hyperfine pass, so the script's exit status is hyperfine's. A parse error, a
missing `_bench`, a runtime error in the cold or warm phase, or a FIFO failure
prints one stderr line inside the harness section; the wall-time table follows,
and the script exits 0. Nothing downstream can tell that the KV half is missing
or partial.

**Fix.** Stop, or at least exit nonzero, when the harness fails.

## SRF-003 - The static-analysis caveat understates what it leaves out

LIB-005 and LIB-006 (notes/bugs-stdlib.md) cover nearby charging claims with
different mechanisms.

**Claims broken**

- `CostAnalysis::caveat()`: "It excludes data-dependent native work in the
  string, pattern, and table libraries".
- docs/budget.md "Static analysis" uses the same wording.
- docs/cli.md says the CLI prints this text as the `--analyze` warning.

**Mechanism.** The wording implies that native charges which are not
data-dependent are counted. They are not. `OP_CALL` is `OpcodeCost::Call`, whose
`static_units` is 0, and the analyzer never looks at the callee. All of these add
0 to `analyze_cost`:

- every math library flat charge (budget.md: "each function charges a flat 1",
  `math.floor`, `math.random` and the rest);
- `table.insert`, `table.remove` and `table.pack` (flat 1 each);
- the up-front `max(1, ...)` minimums of the string functions.

A script whose cost is all `math.*` calls in a loop analyzes to cost 0, so a
script author reading the warning will underestimate flat-charged library calls.
The unit test `static_analysis_excludes_data_dependent_native_work` uses only
`string.upper`, so it pins the narrow wording and misses this.

**Fix.** The analyzer counts only opcode charges, so the caveat should say it
excludes every native library charge, flat or data-dependent.

## SRF-004 - CLI: parse errors in run mode get a cost line, and the error prefix depends on the mode

Minor.

**Claim at issue.** docs/cli.md: "The cost summary is still attempted before a
**runtime** error is reported."

**Mechanism.** src/main.rs's run path folds a compile error into the same
`result` (`compiled.and_then(load).and_then(call)`), so a syntax error in run
mode prints `Cost used: 0` to stderr before `Error: ...`. Nothing ran, and there
was no runtime error. The error text also differs by mode for the same failure:
`--analyze` prints `Error: parsing source: <e>`, while run mode and
`--dump-bytecode` print `Error: <e>`.

**Fix.** Report a compile error before the cost summary, with the same wording in
all three modes. The alternative is to say in the doc that compile errors also
get a cost line.

## SRF-005 - The CLI exit-status statement is wider than what main.rs controls

**Claim at issue.** docs/cli.md: "The CLI only ever exits 0 or 1 (never 124
...)". The comment above `report_error` and a comment in diff_test.sh repeat it.

**Mechanism.** main.rs makes every handled path exit 0 or 1. But `State::guarded`
re-raises any non-marker panic payload (`resume_unwind`), so an internal VM panic
exits 101, and a native stack overflow or allocation failure aborts with a
signal. The property diff_test.sh actually needs, "never 124", still holds,
because none of these produce 124. As written, though, the sentence promises
something about unhandled failures that the CLI does not do.

**Fix.** Narrow it to "the CLI's own exit paths use only 0 and 1", or to "never
124".

## SRF-006 - `scripts/notes_move.py` silently drops a section whose ID appears twice

**Claim at issue.** The header says "Fails before writing anything if a
placeholder has no section, so a typo never silently no-ops." That contract is
met, but the script's stated purpose is to move sections safely.

**Mechanism.** If two source files both hold `## X-001 - ...`, both are cut out
of their sources. In `sections[m.group(1)] = ...` the second overwrites the
first, so after the rewrite the first copy is in neither the destination nor its
source. That is silent data loss.

**Fix.** Refuse duplicate IDs across sources, as the script already refuses
missing ones.

Related: notes_drop.py and notes_move.py both treat a `## ` line inside a fenced
code block as a section heading. Low risk for findings documents, but the
header's "A section runs from its `## ` heading" is literal.

## SRF-007 - `scripts/native_calls.py` always exits 0

Raised as a lateral observation outside the defect bar.

It prints problems and stops, and the brokkr `script_check` relies on
`match = "last-line"` against `ok`. A direct
`python3 scripts/native_calls.py && ...` in any other context reads a failing
inventory as a pass. Exiting 1 whenever problems are printed costs nothing.

## SRF-008 - Hotpath harness: `@` chunk name and truncated `warm_avg_cost`

Raised as lateral observations outside the defect bar.

- **Chunk name.** The harness compiles with `format!("@{}", path)`, the
  reference-Lua convention, but dellingr uses chunk names verbatim. Harness errors
  and `print` locations therefore read `@bench/x.lua:12:`, while the CLI on the
  same file says `bench/x.lua:12:`. No cost or KV is affected.
- **`warm_avg_cost` truncates.** It is `warm_total_cost / iterations` in integer
  division, and `cost_per_us` is derived from the truncated value. For a workload
  whose per-call cost varies (state that grows across calls), the fingerprint
  hides changes smaller than `iterations` units. Either emit the total or emit the
  average with three decimals, like `warm_avg_us`.

## SRF-009 - Maintainer-script smells: `frame_sizes.py --body`, `ab_pairs.py` order, duplicate `lib.sh` wrapper

Raised as lateral observations outside the defect bar.

- **`scripts/frame_sizes.py --body` over-blanks.**
  `re.sub(r"\b[0-9a-f]{4,}\b", "ADDR", body)` also hits any whole word of four or
  more letters from a-f, such as the x87 mnemonic `fadd` or the same kind of word
  inside a demangled `<symbol>`, so two different instructions can diff as equal.
  Matching only hex runs that contain a digit, or anchoring to objdump's address
  columns, would avoid this.
- **`scripts/ab_pairs.py` always runs A before B in every round.** That is
  interleaved, but the slot is fixed, so any effect of a cell's position (thermal
  or cache state left by the previous cell) always lands on the same arm.
  Alternating the order in odd rounds (ABBA) removes that bias for free.
- **`scripts/lib.sh` has two identical wrappers.** `dellingr_bin` only forwards to
  `dellingr_bin_built`, and callers use both names. One can go.

## SRF-010 - Public API leftovers: `doc(hidden)` private module, `pub mod cli`, public test constant

Raised as a lateral observation outside the defect bar.

`#[doc(hidden)] mod patterns;` in src/lib.rs: `doc(hidden)` on a private module
does nothing, which suggests it was once public. Separately, `pub mod cli` puts
CLI-only prefix strings into the library's public API, and
`COST_MODEL_GOLDEN_FINGERPRINT` is a public `doc(hidden)` test constant. Both are
pre-1.0 surface that only the binary and the tests need; they could live in
main.rs and in a `#[cfg(test)]` or test-support location.

## SRF-011 - diff_gate.sh's "only speed" claim ignores the fixed timeout

Raised as a lateral observation outside the defect bar.

diff_gate.sh says that debug versus release "does not change the behavior under
test (identical VM semantics, only speed)". The per-process timeout is the fixed
5 s from lib.sh, so speed alone can change the result: a heavy example that
passes with the release binary can TIMEOUT with the debug one. The same holds for
tests/diff_test.rs, which drives the gate with the dev binary of each check
shape. Worth keeping in mind before trusting a TIMEOUT line from either.

## SRF-012 - The analyzer prints "free" for scopes that can charge at runtime

Raised as a lateral observation outside the defect bar.

The analyzer's per-scope `cost 0 (free)` line is printed for any scope with zero
static cost and no calls. That includes a scope whose only costed work is an open
`SET_LIST` (`return {...}`), which can charge any amount at runtime. This is
consistent with the documented counting rule, but "free" is a stronger word than
"static cost 0".
