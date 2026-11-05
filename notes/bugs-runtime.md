# Runtime findings: VM dispatch, heap, tables, interrupts, standard library

Filed from the 2026-11-04 defect hunt (scopes `compiler-back`, `vm-dispatch`,
`vm-heap`, `stdlib-strings`, `stdlib-core`). Hunt reports are work documents
and may be wrong; verify before acting.

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

The entries below marked Performance describe work the hunters saw in the
code. None has been measured; any change goes through a registered
`brokkr dellingr` workload first.

## RT-019 - str_match builds a fresh MatchState for every start position

Scope: `stdlib-strings`. Performance.

`str_match` builds a new `MatchState` for each start position, which initialises
a 32-entry capture array (about 512 bytes). A failed search over an n-byte
subject does this n times. The hunter observed that every path writes a capture
slot before reading it and that `captures()` reads only slots the successful
path set, and that the `SharedPattern` doc requires state to be fresh per
search, not per start position.

Approaches raised so far (not exhaustive): build the state once per search and
reuse it across start positions.

## RT-020 - string.format appends literal bytes one at a time

Scope: `stdlib-strings`. Performance. Raised by the hunt and again by a fix
wave.

Each literal byte of a format string goes through `append_output`, which runs
`checked_string_growth` and an atomic `state.charge` of 1 per byte. `%%` goes
through `parse_directive`, `format_argument` and a `Vec` allocation to emit one
byte.

Approaches raised so far (not exhaustive): batch literal runs with
`charge_units`, which admits charges exactly as per-byte charging does (the
technique `append_bytes_units` uses in gsub).

## RT-023 - A global-cache unit test fails under the inline-cache differential sweep

Scope: `vm-heap`. Unverified; observed in passing on 2026-11-05 during the
library-table snapshot change (660cb05), which did not touch the code.

`vm::global_env::tests::global_caches_invalidate_on_both_environment_swaps`
fails when run as `brokkr test <name> --sweep inline-cache-differential`.
`brokkr check` does not run that test under that sweep, so the gate is green.
Not established: whether the failure predates 660cb05, and whether the test is
meant to run with cache hits disabled at all (a test asserting cache behaviour
may be wrong by construction under `disable-inline-caches`, in which case the
sweep's scope or the test's feature gate is the thing to state, not a defect in
the caches).
