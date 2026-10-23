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

## SRF-006 - The notes helpers treat a `## ` line inside a code fence as a section heading

`scripts/notes_drop.py` and `scripts/notes_move.py` both start a new section at
any line beginning `## `, including one inside a fenced code block. A finding
that quotes such a line would be cut at it. Low risk for findings documents,
but the headers' "A section runs from its `## ` heading" is literal. Tracking
fence state while scanning would close it.

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
