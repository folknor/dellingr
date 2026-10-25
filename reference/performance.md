# Performance record

Measured numbers over time. Every entry names the commit, the host and the
cargo profile it was taken under; numbers taken under different profiles do
not compare, and neither do `brokkr dellingr` rows stored on either side of a
profile change (`.brokkr/results.db` keeps them, filed under the same
workload names).

## Build profiles

Until 0779c08 the repository's `[profile.release]` set fat LTO and one
codegen unit, and every release benchmark, envelope figure and README timing
taken before then is under that profile. Since the change after 0779c08 the
repository sets no release overrides, so release means Cargo's default
release profile (opt-level 3, thin-local LTO, 16 codegen units), the profile
an embedder most likely builds dellingr under. The current native-stack
figures are in reference/native-calls.md ("Current native bytes per level");
the history of the largest cycle across both profiles is under "Native stack
frames and cycles" below.

## Release profile change, 2026-10-25

Host plantasjen (AMD Ryzen 9 5900X, x86-64 Linux 7.3, governor
`performance`), `rustc 1.101.0-nightly (e9bbadf68 2026-10-12)`, host
rustflags `-Ctarget-cpu=native` with the `wild` linker. The same source,
0779c08, built twice by `brokkr dellingr --bench` (best of 3 launches per
cell, 20 warm calls each): **A** under the old fat-LTO, one-codegen-unit
profile (`--commit 0779c08`, stored rows), **B** under Cargo's default
release profile (the working tree with only the profile removed, run with
`--force` because the tree was dirty, so not stored). Three interleaved
rounds, A B, B A, A B. Figures are the harness's `warm_avg_us`, in ms.

| Workload | A rounds | B rounds | A median | B median | B vs A |
|---|---|---|---|---|---|
| `benchmark` | 103.00, 101.72, 102.01 | 105.19, 105.22, 105.92 | 102.01 | 105.22 | +3.1% |
| `arithmetic` | 67.77, 67.80, 67.58 | 67.48, 67.25, 67.56 | 67.77 | 67.48 | -0.4% |
| `method` | 77.55, 73.58, 74.05 | 74.53, 73.83, 74.48 | 74.05 | 74.48 | +0.6% |
| `mixed` | 71.87, 72.54, 71.52 | 74.68, 77.72, 78.23 | 71.87 | 77.72 | +8.1% |
| `gc_churn` | 78.14, 76.99, 77.35 | 86.69, 86.92, 87.69 | 77.35 | 86.92 | +12.4% |
| `sort` | 100.72, 102.60, 102.64 | 101.33, 101.73, 101.80 | 102.60 | 101.73 | -0.8% |

Every pair of `benchmark`, `mixed` and `gc_churn` is slower under the default
profile (by 2.1 to 3.8%, 3.9 to 9.4% and 10.9 to 13.4% per round), so those
three are real costs of the profile; which functions lost what was not
attributed. `arithmetic`, `method`
and `sort` change by less than their round-to-round spread, with mixed
signs, so they show no effect. No same-binary control was run for these
cells; binary layout differences between the two builds are part of what
this measures.

## Native stack frames and cycles

The log behind reference/native-calls.md's current table: the frame-size
decisions that shaped the native-stack envelope, oldest first. Figures are
bytes of native stack per scenario cycle from `brokkr test
native_bytes_per_level`, or frame sizes from `scripts/frame_sizes.py` (stack
allocation plus pushes). Only the rows that still describe code in `src/` are
kept; rows for functions since removed or replaced are dropped. These are
compiler-dependent measurements, not limits.

### Largest recurring cycle over time

The largest cycle (a `gsub_*_table` or `gsub_pattern_*` scenario, one native
level) in bytes. Release before 0779c08 is under the old fat-LTO,
one-codegen-unit profile and does not compare with the default profile.
Hosts: plantasjen from 4a722cf on; the 2026-10-23 rows name x86-64 Linux only.

| Revision | Dev (also snapshot) | Release | Release + snapshot |
|---|---|---|---|
| 97973d1 (baseline) | 15344 | 3824 | 3872 |
| 22a263a, invocation driver inlined | 15600 | 3776 | 3840 |
| vararg arena on top of that | 15600 | 3776 | 3824 |
| 4a722cf plus the `__index` walk loop | 14720 | 3712 | 3760 |
| df80d81 plus register-native slow paths | 14608 | 3552 | 3520 |
| 41ad774 | 13552 | 3344 | 3360 |
| 0779c08, old release profile | 13680 | 3376 | 3408 |
| 0779c08, Cargo's default release profile | 13680 | 3104 | 3168 |
| 33734f8, Cargo's default release profile | 13680 | 3104 | 3168 |

Every scenario's release cycle shrank under the default profile at 0779c08.
The dev figure drifted from 13,552 (41ad774) to 13,680 (0779c08).

### 2026-10-23: invocation driver inlined into `call_unguarded`

Baseline 97973d1, against unmodified 22a263a, against 22a263a with
`run_invocation` forced inline into both `call_unguarded` monomorphizations.
Release frames, **97973d1 / 22a263a / inlined**, in bytes:

| Function | Frame sizes |
|---|---|
| `call_unguarded`, ordinary | 168 / 120 / 168 |
| `call_unguarded`, entry observer | absent / 120 / 168 |
| `begin_call` | inline / inline / inline |
| `begin_callable_call` | 232 / 232 / 232 |
| `run_invocation` | inline / 168 / inline |
| `Frame::run` | 808 / 776 / 776 |
| `invoke_rust_fn` | 120 / 120 / 120 |
| `call_from_lua` | 120 / 120 / 120 |
| `base_ipairs_iter` | 88 / 88 / 88 |
| `tostring_bytes_unguarded` | 56 / 56 / 56 |
| `tostring_rendering` | 168 / 168 / 168 |
| table sort callback | 184 / 216 / 216 |
| table remove callback | 136 / 88 / 88 |
| gsub callback | 168 / 152 / 152 |

The baseline's `call_unguarded` called `Frame::run` directly, whereas
unmodified 22a263a called a separate `run_invocation`. Restoring inlining
removed 128 bytes, including the extra return address, from each ordinary
release cycle. A Rust-handler cycle saved 80 bytes: its Rust-only call arm has
no live invocation driver, and its now-combined call frame grew by 48 bytes
instead.

The sort callback's 32-byte growth was retained. Its `--calls` and `--body`
diffs showed the new fallible `get_array`, root-copy reservations and
`set_array` paths replacing infallible allocation and cloning, and `asm_diff.py
--slots` showed the changed temporary and spill layout (14 distinct offsets
before, 21 after). Removing those checks would undo ordinary-error handling for
table allocation failures. The smaller dispatch frame cancelled that growth in
the default release comparator cycle; snapshot's compiler layout left that
cycle 16 bytes larger than the baseline until the vararg arena below.

In dev, baseline / inlined `Frame::run` was 6456 / 6568 bytes, `invoke_rust_fn`
264 / 280 and `call_from_lua` 200 / 216, reflecting fallible table operations,
entry observation and the retained result-copy path. The baseline's call and
invocation frames were 344 and 328 bytes; the inlined ordinary call was 712 and
the observed call 760. Folding the driver into the Rust-only arm therefore
increased the two-native-level Rust-handler cycles in dev, although ordinary
cycles shrank by 32 bytes relative to unmodified 22a263a. Both dev feature
shapes still passed the unchanged 4 MiB full-depth envelope.

### Shared vararg arena

Dev (both shapes) and plain release kept every number of the inlined column
above. Release with snapshot saved 16 bytes in every native scenario cycle.
Release frames were unchanged from the inlined column (`Frame::run` 776, 824
with snapshot; both `call_unguarded` copies 168; `begin_callable_call` 232;
`invoke_rust_fn` and `call_from_lua` 120); the only other changed VM symbol was
`return_to_caller`, 72 to 56, which does not stay live across re-entry.

Generic-for iterator bookkeeping was tried both inside the iterator helper and
behind a separate direct-path probe in the dispatch arm. Plain release dispatch
grew from 776 to 792 and 808 bytes respectively, and removing the Rust
iterator fallback's redundant error truncate did not reduce either figure. The
eager record and flush before the iterator call stays; the comment at its site
in `src/vm/frame.rs` points here.

Removing the Rust callee from the stack at a call also stays. The contiguous
callback ABI exposes every slot freed by removing the callee; retaining it
below the arguments would refuse a push one slot earlier at the global stack
cap. Moving the function elsewhere still relocates the arguments, and a
separate result destination alone cannot recover that slot during the callback.

### 4a722cf: `__index` table steps as a loop

Measured on plantasjen, 4a722cf against the working tree on top of it. An
`__index` table step became a loop in one outlined function that also takes the
handler's depth unit and pushes its call window, so the frames that stay live
across the handler call are only the caller's: a table chain no longer recurses
natively. `Frame::run` was unchanged (776, 824 with snapshot). Two
intermediate shapes were measured and dropped: a recursive table step with a
pre-guard terminal check grew every release cycle that reaches an `__index`
function by 96 bytes (the dispatcher gained a second caller and an outlined
frame), and a first loop that returned the final handler for its caller to push
and call grew them by 80 (the returned handler, receiver and unit count stayed
live across the call).

### df80d81: register-native slow paths

Measured on plantasjen on top of df80d81, when the table and length
instructions stopped copying operands above the frame. Each read, store and
length became one outlined resolver (`State::resolve_index`,
`State::store_field` and `State::resolve_newindex`, `State::resolve_length`)
that returns before any handler runs; the handler call is a 24-byte release
frame of its own (`index_call`, `newindex_call`, `len_call`). `__newindex`
table chains and string `__index` handlers became loops too. Release
`Frame::run` went 776 to 728 (824 to 696 with snapshot); `get_table_unguarded`
120 to 264, absorbing the observed `call_unguarded` copy; `call_from_lua` 120
to 136, the one frame on these paths that grew (not attributed: it inlines
admission, whose only change is the `FRAME_HEADROOM` term, and an equivalent
spelling of that check measured the same). The non-reentrant resolvers were
`resolve_index` 280, `store_field` 184, `resolve_newindex` 216 and
`resolve_length` 136. Representative release cycles, before / after:
`index_field` 1520 / 1040, `newindex_field` 1392 / 944, `length` 1168 / 944,
`host_call` 1296 / 1264, `gsub_pattern_table` 3712 / 3520; dev `index_field`
9296 / 7744. The dev `tfor_lua` cycle grew 16 bytes: the iterator call now
keeps its call base to deliver results into the loop registers.

The first build of this shape let the compiler inline the closure, template
constructor and concatenation bodies into `Frame::run` (their buffers, capture
list and runtime handle), which grew it to 808 in plain release. They were made
`inline(never)`; no dispatch cycle pays for them.

### 0779c08: Cargo's default release profile

The repository dropped its `[profile.release]` overrides (fat LTO, one codegen
unit), so release sweeps build Cargo's default release profile (thin-local LTO,
16 codegen units). Measured 2026-10-25 on plantasjen on top of 0779c08, with
`rustc 1.101.0-nightly (e9bbadf68 2026-10-12)` and that host's global rustflags
(`-Ctarget-cpu=native`, the `wild` linker through clang). Every scenario's
release cycle shrank: the largest went from 3,376 to 3,104 bytes (3,408 to
3,168 with snapshot), `index_field` from 832 to 528, `gsub_pattern_table` from
3,344 to 3,072, `tfor_lua` from 704 to 640. Dev is unaffected by the profile
change. Frames were not re-attributed with `scripts/frame_sizes.py` for this
change. The wall-clock side of the same change is in the section above.
