# Verification: native stack and limits

Findings from the verification hunt about the native-stack envelope and the
limit tests: `tests/native_stack.rs`, `reference/native-calls.md` and
`reference/native-calls.toml`, `tests/common/stack.rs`, the at-cap and
iterative-walker tests, and the claims in `docs/limits.md`. Each entry names the
scopes whose hunters reported it.

```
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
```

---

## VN-006 - The release envelope is validated for a profile no embedder ships

Reported by: native-stack-errors.

`[profile.release]` here is fat LTO with one codegen unit. Cargo profiles of a
dependency come from the embedding workspace, so the game builds dellingr with
its own profile, typically Cargo's default release (thin-local LTO, 16 codegen
units), whose inlining and frame sizes differ. `docs/limits.md` says other
configurations are the embedder's to validate, which is honest, but the
repository's own release envelope certifies an artifact nobody links.

Fix: run the release envelope sweep under Cargo's default release settings too
(a custom profile `release-default` inheriting from `release` with the defaults
restored), or state the envelope for that profile instead.

Enforceable: yes, as a `[[check]]` shape.

Decision (2026-10-24): change the repository's defaults instead. Delete
`[profile.release]`'s `lto = "fat"` and `codegen-units = 1` (`opt-level = 3`
is already the default, so the section goes), so the existing release sweeps
certify the profile an embedder most likely builds. Consequences to handle with
the change: every `brokkr dellingr` number shifts and comparisons across the
change measure profile and code together; measured numbers in
`reference/performance.md` and the README state the profile they were taken
under; the release envelope and the per-cycle figures in `docs/limits.md` and
`reference/native-calls.md` are re-measured. No opt-in fat-LTO profile until
something needs one.

## VN-007 - The envelope depends on build inputs the repository neither pins nor records

Reported by: native-stack-errors, corpus-harness.

The first two findings (no toolchain pin; host rustflags outside the
repository) were decided: no pin and no override, and `docs/limits.md` now says
the envelope is verified on the developer's machines with their toolchains and
global cargo configuration. What remains is the third:

- `reference/native-calls.md` says "All 57 scenarios were rerun"; the inventory
  has 55 scenarios (57 matches the probe's rows, which include two `chain_`
  rows). The file has become a measurement log (four dated sections, several
  superseded, rows for functions that no longer exist) inside a `reference/`
  document that must be true, and a reader cannot tell which table is current.

Fix: move the measurement history to `reference/performance.md` (the folder
table's home for measured numbers over time), leaving native-calls.md with the
guard and one current table; drop the scenario count; record `rustc -V` with
measurements.

Enforceable: editorial.

Undecided.

## VN-009 - At-cap probes refill a million slots per attempt

Reported by: native-stack-errors, runtime, library.

`frame_reservation.rs::run_at_cap` and `index_depth_routes.rs::call_near_cap`
find the highest admitting stack height by starting at `MAX_STACK_SIZE - 2` and
lowering by one; every attempt does `set_top(fill)` from 0 (one million
`Val::Nil` pushes, one at a time, in `State::set_top`) then `set_top(0)`. The
attempt count is the probe's frame size plus `FRAME_HEADROOM`.
`frame_reservation.rs` does this for 18 bodies plus three more;
`index_depth_routes.rs` calls `call_near_cap` about a dozen times.
`host_error_reporting.rs` (`execution_stack_preflights_notify_once`, four cases;
`attempt_result_padding`, three modes) and `host_call_preflight.rs` fill the same
million through public `push_nil`. In `src/vm/tests.rs`, `fill_to_one_below_cap`
and its users, `host_table_api_atomicity.rs`'s `get_table_preserves_the_key_*`
and `raw_reads_replace_the_key_even_at_the_stack_cap` (looped four times in one
test), the `string_metatable_dispatch` stack-cap tests (`set_top(999_999)`), and
`get_table_preserves_the_key_at_the_lua_call_depth_limit` (recursing to the real
`MAX_CALL_DEPTH`) do the same at full scale. Estimated on the order of hundreds of
million-slot fills per sweep in dev, both dev sweeps, likely seconds, plus a
memory peak when libtest runs a dozen in parallel; not measured.

Fix: fill once and lower the height by popping one slot per failed attempt (a
failed host `call` leaves the filler as it was, which these tests already assert),
or compute the exact height from the probe's `max_stack`, which
`Program::disassemble` prints. Lateral: `set_top`'s growth arm could `resize`
after its preflight instead of a push loop. The test-only `Limits` override in
rewrites.md is the alternative the runtime and library hunters prefer; the
native-stack hunter argues against it.

Enforceable: no; it is a fixture shape.
