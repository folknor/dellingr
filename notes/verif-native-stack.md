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

## VN-005 - `cfg!(debug_assertions)` stands in for the build profile

Reported by: check-machinery, corpus-harness, native-stack-errors, runtime.

`tests/common/stack.rs` picks 4 MiB or 2 MiB by `cfg!(debug_assertions)` and
says it is "not a definition of unoptimized". The brokkr manual calls the
heuristic unreliable under profile overrides, and nothing ties it to
`Cargo.toml`. A `[profile.release] debug-assertions = true` (a common hardening
move), or a dev profile with `opt-level = 3`, or a custom profile with debug
assertions and optimisation, silently selects the other envelope: the release
sweeps would test the looser 4 MiB envelope and still report "release". Neither
exists in the repository today, so the guard holds; it is keyed on a proxy and
reports nothing when the proxy stops matching.

Fix options: have the release sweeps export a marker through `[[check]] env` and
refuse when it disagrees with `cfg!(debug_assertions)`; or compare against
`BROKKR_TEST_BIN_DIR`, which ends in `/release` for those sweeps; or a
`build.rs` that exports cargo's `PROFILE` as a `cfg` so the two repository
profiles are named rather than inferred.

Enforceable: yes.

Decision (2026-10-24): no action. Owed before removal: a comment at
`LIMIT_TEST_STACK_BYTES` in `tests/common/stack.rs` stating that the
repository's profiles keep `debug_assertions` and the profile in step, and that
a profile or host override breaking that link is accepted as unguarded.

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

- Toolchain: `reference/native-calls.md` and `docs/limits.md` record bytes per
  cycle and frame sizes with commit and host, but not the rustc version, and the
  repository pins no toolchain (no `rust-toolchain.toml`). Frame sizes are a
  compiler property first; a toolchain bump can flip `release-native-stack`
  red or green with no source change, and "this repository's release build
  settings" is not reproducible without it.
- Host rustflags: `~/.cargo/config.toml` on plantasjen sets
  `-Ctarget-cpu=native` (wider vector registers, different spill decisions),
  links with `wild` via `clang`, sets `[profile.dev] debug =
  "line-tables-only"`, and installs `brokkr-rustc-guard` as the rustc wrapper.
  None is in the repository. `tests/common/stack.rs` states the envelope as "4
  MiB with this repository's dev settings and 2 MiB ... with its release
  settings", but the frames it bounds are measured under these host flags; on a
  host or CI without them frame sizes differ. Native tuning also means the
  crmath FMA path is the only one corpus runs exercise on this host (see the FMA
  mode entries in verif-assertions.md).
- `reference/native-calls.md` says "All 57 scenarios were rerun"; the inventory
  has 55 scenarios (57 matches the probe's rows, which include two `chain_`
  rows). The file has become a measurement log (four dated sections, several
  superseded, rows for functions that no longer exist) inside a `reference/`
  document that must be true, and a reader cannot tell which table is current.

Fix: pin the toolchain (`rust-toolchain.toml`) so the envelope is stated for a
named compiler, and record `rustc -V` with every measurement; put the flags the
envelope depends on in a repository `.cargo/config.toml` (or `[profile]`
settings) so they travel with the code, or state in `docs/limits.md` that the
envelope was verified under plantasjen's global rustflags and add a sweep without
them; move the measurement history to `reference/performance.md` (the folder
table's home for measured numbers over time), leaving native-calls.md with the
guard and one current table; drop the scenario count.

Enforceable: the pin is structural; the rest is editorial.

Decision (2026-10-24): no toolchain pin and no flag override. dellingr is
developed on several hosts; `docs/limits.md` states that the envelope is
verified on the developer's machines, with their toolchains and global cargo
configuration, and that it stays that way until decided otherwise. The
`native-calls.md` cleanup (third bullet) is not decided yet.

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
