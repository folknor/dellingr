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
native-calls.md says it checks.

## SRF-010 - Public API leftovers: `doc(hidden)` private module, `pub mod cli`, public test constant

Raised as a lateral observation outside the defect bar.

`#[doc(hidden)] mod patterns;` in src/lib.rs: `doc(hidden)` on a private module
does nothing, which suggests it was once public. Separately, `pub mod cli` puts
CLI-only prefix strings into the library's public API, and
`COST_MODEL_GOLDEN_FINGERPRINT` is a public `doc(hidden)` test constant. Both are
pre-1.0 surface that only the binary and the tests need; they could live in
main.rs and in a `#[cfg(test)]` or test-support location. The second
resolution wave also made `State::pattern_compilations` available in release
builds; check whether that counter belongs in the public API at all.
