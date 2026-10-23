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

# Compiler defects: front end, codegen, verifier, encoding

Reopened for laterals from the resolution waves of the 2026-10-23 defect hunt.
Earlier compiler IDs (CMP-001 to CMP-012) were all resolved and are not reused.

## CMP-013 - The register-limit error line is sticky

Lateral from the third resolution wave's review. `TooManyRegisters` now carries
a source line from a codegen `error_line` hint instead of the function's end,
but the hint is set at many constructs and never cleared. A register overflow in
a construct that does not set it (a `local` statement's expression list, for
example) reports the line of the last construct that did. Better than the
function's last line, but the CHANGELOG's "the line of the construct that ran
out of registers" overstates it. Either set the hint at every construct that
can reserve registers (or at `reserve` itself, from the current stamp), or
soften the CHANGELOG wording.
