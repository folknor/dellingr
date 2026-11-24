# Performance record

Each measurement records the commit, host and Cargo profile. Compare runs
within the same profile, whether they come from the current `brokkr bench`
script benches or the historical `brokkr dellingr` command. Historical rows
in `.brokkr/results.db` retained the same workload names across profile changes.

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

## Pattern arguments read in place, 2026-11-05

`string.find`, `match`, `gsub` and `gmatch` stopped copying a string subject
and pattern into owned buffers on every call (and `gmatch` stopped
re-interning them); they read them in their stack slots (`vm/string_arg.rs`).
The change against its parent ca5f49f on plantasjen (as above), default
release profile, `brokkr dellingr --bench` cells (best of 3 launches, the
harness's elapsed ms), base and change interleaved:

| Workload | ca5f49f | Change |
|---|---|---|
| `large_subject` | 4901 | 2274 |
| `literal_find` | 4469, 4468, 4446 | 4466, 4466, 4473, 4436 |
| `patterns` | 2277, 2268, 2265 | 2274, 2262 |

`large_subject` is a probe registered for this change: a 64 KB subject and
calls that stop after a few bytes of matching, so the per-call copy is most
of the work. On the short subjects of `literal_find` and `patterns` the
removed copies are below the noise. A first version of the change validated
the gsub subject's slot twice per matcher run and measured `patterns` at
2288, 2313 and 2300; reading one argument through its own helper removed
that. An early `find`-only prototype measured `literal_find` at 4156 and
4124, which the full change does not reproduce; it was not a same-binary
control, so treat that gap as layout.

## Owned results interned without a second copy, 2026-11-24

Library results built in an owned `Vec` (`gsub`, `upper`, `lower`,
`reverse`, `sub` of a number, `string.format`, `table.concat`, `tostring` of
a non-string, and `..`) go into the string pool through `push_owned_bytes` /
`alloc_string_owned`: an intern miss converts the buffer with
`into_boxed_slice` instead of copying it from a slice. Same probe, collection
point, identities and resident length, so not replay-visible. The table
below, against its parent 81090d3 on bygg (`brokkr bench wall`, best of 3,
ms, two pairs, the second in reversed order), is not a matched pair: the
parent arm was a `--commit` worktree build and the change arm the live dirty
tree, which differ in more than the commit. Every row of it is qualified by
that, the gains as much as the slowdowns.

| Workload | 81090d3 | Change |
|---|---|---|
| `format_literals` | 742, 767 | 718, 716 |
| `mixed` | 2230, 2234 | 2190, 2194 |
| `large_subject` | 1283, 1270 | 1284, 1283 |
| `patterns` | 1177, 1180 | 1194, 1196 |
| `sequence_len` | 2303, 2312 | 2373, 2377 |

A diagnostic variant that took only buffers with capacity equal to length
and copied the rest (also a live-tree cell) measured `format_literals` 757
and `sequence_len` 2416. `large_subject` shows no gain: its `gsub` output is
mostly an intern hit, and the copy a miss saves is small against its scan.

Re-measured with both arms as `--commit` builds, interleaved `--rounds`,
one launch per cell, on bygg the same day. eb33341 against 81090d3:
`sequence_len` faster in 6 of 6 rounds, median paired -80 ms (invocation
ec5c73c0), so the slowdown above was the unmatched arm and is withdrawn;
`patterns` slower in 4 of 6 (+13 ms) and then 11 of 12 (+11.5 ms,
invocation c5f3b532), so about 1% there is real for that binary pair, cause
unattributed.

At fe88e6a, against a local diagnostic commit that routed
`State::alloc_string_owned` through the borrowed copy (owned path dead), 8
rounds each: `mixed` is 64 ms (2.9%) faster with adoption, 8 of 8 (its `..`
results are intern misses, so adoption saves a copy on each); `patterns` is
15 ms and then 9 ms slower with adoption (8 of 8, 5 of 8); `format_literals`
about 6 ms slower (5 of 8, 7 of 8); `large_subject` tied; `sequence_len` 27
ms faster with adoption (6 of 8) although its `table.concat` results are all
intern hits. Adoption is kept for the `mixed` gain. A third arm adopting only
exact-capacity buffers ran `format_literals` 27 ms slower than both pure
arms (8 of 8). It changes the executed path, so it is not a layout control,
but a hybrid landing outside both behaviours it mixes leaves wall time at
this size unable to attribute the `patterns` cost.

## Short interned strings inline, 2026-11-24

A string of at most 22 bytes is stored inside its arena slot rather than in
a separate `Box<[u8]>`; the slot grows from 32 to 40 bytes (pinned by a
const assertion in `vm/object.rs`). Identity, collection points and
resident accounting are unchanged. Release wall, best of 3, two matched
`--commit` rounds of b6e2bfa against the change on bygg, second round:
`mixed` 2214 to 2183 ms, `patterns` 796 to 782 (first round 794 to 775),
`fill` 1515 to 1488, `format_literals` 644 to 640 (one round). Every pair
moved the same way, but each delta is inside the layout noise floor, so
this records no attributed speedup; `state_new_us` in the records fell
from about 42 to 28 us.

## gmatch iterators hold their compiled program, 2026-11-24

A gmatch step used to resolve its program from the pattern cache on every
call (a hash lookup, an Arc clone and two string checks); the iterator
object now holds the program, taken from the cache at creation or load.
Release wall on bygg, `brokkr bench wall` best of 3 in ms, both arms built
with `--commit`, two interleaved pairs, against the parent dbf3841:

| Workload | dbf3841 | Change |
|---|---|---|
| `gmatch_bytes` | 4765, 4648 | 4467, 4221 |
| `failed_scan` | 966, 955 | 932, 925 |
| `patterns` | 775, 779 | 769, 764 |

`gmatch_bytes` is a probe of one-byte successful steps, so its ratio
overstates the effect on real scripts. No same-binary control was run; the
`patterns` delta is inside the layout spread. Every `native_bytes_per_level`
row is unchanged once `cached_pattern_at` is kept out of line.

## Typed gmatch iterators, 2026-11-24

A `string.gmatch` iterator became a typed heap object (`GmatchIter`) whose
call runs the library step directly, in place of a Lua closure from a
compiled wrapper chunk over a private state table. Each step used to admit
the wrapper's Lua frame, call the step as a nested Rust function, intern
six field keys and make four raw table reads and two writes; it now reads
and writes the record. Creation allocates one object instead of the
wrapper's closures and table, and makes no call from Rust. Against its
parent 7cad369 on bygg, `brokkr bench wall` (best of 3, ms), two interleaved
pairs:

| Workload | 7cad369 | Change |
|---|---|---|
| `patterns` | 1204, 1189 | 802, 800 |
| `failed_scan` | 2261, 2266 | 946, 955 |
| `large_subject` | 1292, 1294 | 1089, 1092 |

The pairs were taken on an intermediate build whose `begin_call` had two
call sites for `invoke_rust_fn`; the committed single-site shape measured
788 (`patterns`), 950 (`failed_scan`) and 1.1 s (`large_subject`) in one
cell. `failed_scan` runs many short gmatch scans, so the per-step state
traffic was most of its time; `large_subject` takes one step per iterator,
so its gain is creation. As a control, `benchmark` (3581 to 3556) and
`method` (535 to 524) did not move. No same-binary control was run; the
gmatch deltas are far outside the layout spread recorded elsewhere in this
file. In `native_bytes_per_level`, the release frame grew 16 bytes on the
index-key scenarios and shrank 16 on `host_invoke`, everything else equal
(release + snapshot also shrank 48 on the `gsub_pattern_*` rows);
the dev frame grew 96 to 256 bytes a level, from the iterator's dispatch
arms in `begin_call` and `instr_tfor_call`.

## Batched matcher kernels, 2026-11-17

Every matcher step polled the interrupt flag and entered the cost meter,
once per subject byte in a greedy run and twice per failed start position.
Five kernels now scan first and settle the steps they covered in one
`consume_units` batch, after one poll: a greedy class run (`max_expand`), a
run of single atoms in `match_at`, the start positions an unanchored search
must fail at (a program opening with capture items and an `x` or `x+` atom,
`StartSkip`), the `%b` scan and the backreference compare. Each kernel only
inspects bytes whose steps `CostMeter::admissible_units` admits, and hands
the exact position back to the scalar step at the budget edge, so cost,
matches, captures and errors are unchanged, budget refusal included
(`kernels_keep_the_scalar_steps_at_every_budget` pins a fingerprint taken
from the step-per-byte matcher). An explicit continuation stack in place of
the native recursion was considered and not taken: recursion happens per
backtracking point, not per byte.

Bygg, `brokkr bench wall` (best of 3), base 895cf3b and the change,
interleaved pairs, ms:

| Workload | 895cf3b | Kernels |
|---|---|---|
| `failed_scan` | 2785, 2809 | 2251, 2285 |
| `patterns` | 1309, 1320 | 1193, 1188 |

## Matcher capture state per search, 2026-11-05

`str_match` built a fresh `MatchState` for every start position it tried,
and with it a 768-byte capture array (32 slots of 24 bytes): the release
build called `memset` for 768 bytes before every `match_at` entry, so a
failed search over n bytes paid it n times, and gsub and gmatch paid it
again on every match. The change builds one `MatchState` per search, shares
its slots across start positions without a reset (sound because a path
reads only slots it wrote itself; `MatchState::captures` states the rule),
drops `CaptureEnd`'s restore on failure for the same reason, and on success
copies the program's slots into the matcher (`CaptureSlots`), which
converts a slot when it is read and refuses reads after a failed search.

A first version kept no per-search array at all: the matcher's own slots,
reached from `MatchState` through a `&mut` borrow. In release that second
live pointer grew `match_at`, which recurses natively up to `MAXCCALLS`,
from 168 to 200 bytes per level (and `min_expand` from 72 to 120), and it
measured slower than the shipped shape, which keeps `match_at` at 168 and
`min_expand` at 72. `str_match`'s own frame stays 888 bytes; it is one
level, not part of the recursion, and returns before gsub calls a
replacement.

Plantasjen (as above), default release profile, `brokkr dellingr --bench`
cells (best of 3 launches, the harness's elapsed ms), base and both shapes
interleaved:

| Workload | 58e2808 | Matcher-owned slots | Per-search slots (shipped) |
|---|---|---|---|
| `failed_scan` | 4938, 4940, 4946 | 4582, 4557, 4556, 4572 | 4518, 4495, 4506 |
| `patterns` | 2256, 2269, 2258 | 2184, 2190, 2189, 2189 | 2160, 2161, 2176 |
| `large_subject` | 2308, 2264, 2302 | 2298, 2273, 2276, 2280 | 2260, 2247, 2267 |

`failed_scan` is a probe registered for this change: searches over a
few-hundred-byte subject that try nearly every start position. The shipped
shape is 8.8% faster there and 4.3% faster on `patterns` (medians); `large_subject`,
whose searches stop after a few bytes, moves within its spread. The native
cycles in reference/native-calls.md moved with `LuaPattern`'s layout:
release `gsub_pattern_function` went from 3184 (3248 with snapshot) at
58e2808 to 3168 (3216).

## `string.format` literal runs batched, 2026-11-05

`string.format` appended every literal byte of its format string as its own
growth check and atomic charge, and sent `%%` through the directive parser
and `format_argument` to emit one byte. The change batched each run of
literal text between `%`s into one copy with the same per-byte charging
semantics, using the batched appender that `gsub` used for template
literals at the time. It also treated `%%` as literal text and batched
literal runs on the first `gsub` template application. The change
preserved costs, the exhaustion point and the cap boundary, as pinned by
the per-byte literal tests of the time; cost model 9 later removed per-byte
literal charges and those tests with them.

Plantasjen (as above), default release profile, `brokkr dellingr --bench`
cells (best of 3 launches, the harness's elapsed ms), base and change
interleaved:

| Workload | d98e154 | Change |
|---|---|---|
| `format_literals` | 1847, 1847, 1861 | 1204, 1186, 1262 |
| `patterns` | 2162, 2153, 2168 | 2134, 2138, 2141 |
| `failed_scan` | 4502, 4498, 4501 | 4521, 4457, 4462 |

`format_literals` is a probe registered for this change: templates of mostly
literal text with a few `%d`, `%s` and `%%`. It is 35% faster (medians);
`patterns` and `failed_scan` move within their spread. The `format` native
cycle shrank (release 1520 to 1472, 1488 with snapshot; dev 12912 to
12528). Release default `gsub_*` cycles grew 16 bytes (`gsub_pattern_function`
3168 to 3184, back to its 58e2808 figure); dev and release + snapshot are
unchanged. The cause of the gsub move is unproven; marking the appender
`#[inline(never)]` did not change it, so it was left inlinable.

## `string.format` renders into one buffer, 2026-11-24

`string.format` copied its template per call, allocated a `raw` Vec per
directive, rendered each directive into a Vec of its own (plus a digit
`String` per integer) and then copied that into the output. The change reads
the template in place from slot 1 (reacquired after any `__tostring`),
keeps a directive's bytes as a range, finds the next `%` with one scan per
literal run, and writes every conversion straight into the output buffer
with digits in fixed scratch. Each directive still raises its rendering
errors before the aggregate cap check, `%q` keeps its piece-relative
checks, and `%p` resolves its identity before the size check, so results,
errors, reported sizes and charges are unchanged.

bygg, release, `brokkr bench wall --workload format_literals`, best of 3,
cells interleaved against eb33341: 731, 736 ms to 648, 646 ms (about 12%).
The release `format` native cycle went from 1568 to 1408 bytes (measured
under a newer compiler than the previous table; see
[native-calls.md](native-calls.md)).

## Sequence library reads off the stack, 2026-11-17

`table.concat` and `unpack` read each element straight from the table
when the raw value is non-nil or the metatable has no `__index`, and
`table.concat` appends string bytes from the pool and numbers through
`number_to_text` with no per-element owned copy (`sequence_raw_get`,
`concat_append_raw` and `push_sequence_raw` in `vm/table_ops.rs`). Only a
raw miss with an `__index` handler takes the key push and
`get_table_unguarded`. `table.pack` stores its arguments into the table
directly, and raw `table.insert`/`table.remove` compute the border once.
The change against its parent e0441d3 on bygg, `brokkr bench wall` (best of
3), base and change interleaved:

| Workload | e0441d3 | Change |
|---|---|---|
| `mixed` | 2.7 s, 2.7 s | 2.2 s |
| `sequence_len` | 3.6 s, 3.6 s | 2.7 s |

`mixed` joins a 500-string table once per kernel call; `sequence_len` is
short insert, remove, unpack and concat calls on plain tables and on tables
with an unrelated metatable or a `__len` handler. Native bytes per level
(`native_bytes_per_level`): release `remove_*` rows grew 16 bytes, the other
release rows are unchanged; in the dev profile the `concat_*` rows shrank
128 bytes and the `unpack_*` rows grew 48.

## Dense-prefix proof for the length operator, 2026-11-18

A table keeps a `dense` bit, a proof that its live array keys are exactly
`1..=array_live`, so `array_len` returns that count instead of
binary-searching `1..=vlen` when the last array slot is nil. Tail pushes
and pops keep it, as do shifts of a dense prefix by `table.insert` and
`table.remove`; other writes that might leave a hole clear it, and it
comes back when the array empties. The answer is the border the search
would return, so nothing replay-visible moves. The bit shares a word with
the hash part's tombstone count (`TombstonesAndDense`), because a separate
field grew every table, and so every object slot, by 8 bytes. Against its
parent 5c4f6e5 on bygg, `brokkr bench wall` (best of 3), interleaved
pairs of the packed layout:

| Workload | 5c4f6e5 | Change |
|---|---|---|
| `deep_stack` | 441 ms | 331 ms |
| `record_tables` | 2.4 s, 2.5 s | 2.6 s, 2.6 s |
| `sequence_len` | 2.3 s | 2.3 s |
| `fill`, `queue` | 1.5 s | 1.5 s |
| `rekey` | 1.0 s | 1.0 s |
| `list_literals` | 2.6 s | 2.6 s |

`deep_stack` pushes and pops 32 entries on a stack 500 deep whose logical
array once reached 1024 keys, taking `#s` on every operation.
`record_tables` is about 5% slower (WARM blocks 47 ms against 50 ms, in
every block). It never reaches the array part; its executed differences
are code generation around the template constructor, which then rebuilt
each instance's hash part key by key: `push_new` went outlined and staged
key and value through the stack. Forcing `push_new` inline did not recover
it (2.6 s). The next entry removes that per-instance build.

## Constructor templates copied from a prototype, 2026-11-18

A named-field constructor template used to build every instance's hash
part key by key: hash, probe and insert each key as a nil placeholder.
Each bytecode runtime now builds one `TemplatePrototype` per template on its
first execution and every instance copies it: the slot vector at the
reserved capacity, the hashbrown index by `clone`, which keeps the bucket
layout. Slot order, backing bytes charged to the collector, snapshots and
costs are those of the key-by-key build. Taken on bygg against fb1081f
(release, `brokkr bench wall`, best of 3, the change run from a dirty tree),
interleaved with 5c4f6e5:

| Workload | 5c4f6e5 | fb1081f | Change |
|---|---|---|---|
| `record_tables` | 2.4 s | 2.6 s | 2.0 s, 2.0 s |

On `examples/alloc/record_tables.lua` with 2000 warm iterations, `perf
stat` against fb1081f: cycles 2.53G to 2.00G, instructions 10.44G to
8.79G, `ex_no_retire.load_not_complete` 0.87G to 0.64G. This also retires
the fb1081f regression above: the code whose generation shifted no longer
runs per construction.

## Stores through a metatable without `__newindex`, 2026-11-18

A store into a table with a metatable used to look the key up on the
receiver to decide whether `__newindex` runs, read `__newindex` from the
metatable when the key was absent, then look the key up again inside the
insert. A metatable now carries a proof, learned from a nil `__newindex`
lookup and forgotten whenever its hash part may gain a live key, that it
holds no `__newindex`; a store through it is the plain table's one insert.
New workload `object_fields` (objects built field by field after
`setmetatable({}, Class)`). On bygg against f827b9f (release, `brokkr bench
wall`, best of 3, the change run from a dirty tree), two interleaved pairs:

| Workload | f827b9f | Change |
|---|---|---|
| `object_fields` | 3.8 s, 3.8 s (warm call 76.4 ms) | 3.5 s, 3.5 s (warm call 69.8 ms) |
| `method_polymorphic` | 3.5 s | 3.6 s (warm call 70.0 ms both) |
| `method` | 2.9 s | 2.8 s |
| `fill` | 1.5 s | 1.5 s |

## Inherited routes under a writer-side epoch, 2026-11-18

A field read through more than one `__index` table used to walk the whole
chain on every call; a validate-on-hit route cache had been built and
refused because each hit still resolved every table on the path. A site
now caches the route behind one heap-wide route epoch: the walk marks
every table it read for a link or a miss, and a write that may change a
route moves the epoch when it reaches a marked table (which writes count
is in the next two sections), so a hit checks the receiver's own miss, its metatable
identity and the epoch, takes the route's depth units and reads the
terminal slot. The warm one-level method hit also stops resolving the
metatable twice for `C.__index = C`. On bygg against c429707 (release,
`brokkr bench wall`, best of 3, the change run from a dirty tree), two
interleaved pairs:

| Workload | c429707 | Change |
|---|---|---|
| `method_chain` | 2.6 s, 2.6 s | 2.2 s, 2.1 s |
| `method_chain_wide` | 3.0 s, 3.1 s | 2.1 s, 2.2 s |
| `method` | 3.0 s, 3.0 s | 2.8 s, 2.8 s |
| `method_polymorphic` | 3.6 s | 3.5 s |
| `object_fields` | 3.5 s | 3.5 s |
| `write` | 2.3 s | 2.3 s |

## Route-preserving writes to class tables, 2026-11-18

The route epoch above moved on every mutable handout of a marked table,
so a class written in a loop retired every route on each write. Two
writes no longer move it: the terminal table is not marked (the entry
names it and reads its version-carrying slot live), and a cached
SET_FIELD that overwrites a live slot takes `GcHeap::overwrite_live_slot`,
which leaves the epoch alone (the set cache is never filled for an
`__index` store, and a live overwrite creates no key and changes no
link). `method_chain_counter` bumps a static counter on the receiver's
metatable on every two-level method call. On bygg against 3adbfb7
(release, best of 3, the change run from a dirty tree), interleaved:

| Workload | 3adbfb7 | Change |
|---|---|---|
| `method_chain_counter`, `wall` | 3.1 s, 3.2 s | 3.1 s, 3.0 s |
| `method_chain_counter`, `kernel` warm average | 355 us | 312 us |
| `method_chain`, `wall` | 2.2 s | 2.2 s |
| `write`, `wall` | 2.3 s | 2.3 s |

Under `--hotpath`, `resolve_index` went from 49 ns to 12 ns average over
the same 422006 calls and the cold walk's `Table::get` calls disappeared:
the read now hits the route. The thrash was cheap to begin with, since a
two-table cold walk is short; deeper or map-storage routes pay more.

## Route retirement classified per mutation, 2026-11-18

Every other store still retired every route at the handout, even a live
overwrite: an uncached or polymorphic SET_FIELD, SET_TABLE with a string
key, `rawset`, host stores and `__newindex` table stores. `TableEditor`
now classifies each mutation instead: on a marked table, a non-nil store
under a key other than `__index` that finds the key live writes that entry
and leaves the epoch alone; anything else retires first. An unmarked
table pays one bit test, as before. Only a raw `GcHeap::as_table` borrow
(`setmetatable`, the sort writeback) still retires at the handout. The
cached global store refuses a marked global table rather than classify
inline, since a call there grew the dispatch loop's release native frame
by 16 bytes; its keyed fallback classifies. Release native frames are
unchanged; the dev build grew 32 bytes a level. No registered workload
writes a route table through these paths, so the gain is the cold walk
each such write used to force. On bygg (A 8db1c49, B the change from a
dirty tree, best of 3): `write` 2.4 s, 2.4 s; `global` 884 ms, 896 ms;
`object_fields` 3.4 s, 3.4 s; `method_chain_counter` 1.6 s, 1.5 s.

## Closure creation in one allocation, 2026-11-17

A closure is stored inline in its heap slot (no `Box`), its runtime owns
its bytecode (one handle instead of two), and `reg_closure` collects its
captures straight into the shared `Arc<[UpvalueRef]>` with no staging
`Vec`: a capturing closure went from three allocations, one immediate free
and two reference-count increments to one allocation and one increment; a
capture-free one (every `string.gmatch` call then made one, its wrapper's
main closure) allocates nothing
of its own. The cache sweep's chunk-retention set is a reused hash set kept
by the heap instead of a fresh `BTreeSet` per collection. Against its parent
851d129 on bygg, `brokkr bench wall` (best of 3), base and change
interleaved as A B B A:

| Workload | 851d129 | Change |
|---|---|---|
| `closure` | 3.5 s, 3.5 s | 2.9 s, 2.9 s |
| `gc_churn` | 3.2 s, 3.2 s | 2.9 s, 2.9 s |
| `arithmetic` | 2.9 s | 2.9 s |
| `method` | 3.1 s | 3.2 s |

`arithmetic` and `method` are one pair each, a check that the call path
(which now reads a closure's bytecode through its runtime) did not regress;
the `method` step is inside the layout-noise band.

## Capture lists of zero or one inline, 2026-11-18

`Closure` and `Frame` hold a two-word `Captures` instead of an
`Arc<[UpvalueRef]>`: no capture or one capture is stored inline and cloned
by copy, two or more share one `Arc` slice as before. A one-capture closure
now allocates nothing of its own, and admitting a call to a closure with at
most one capture (every captureless method, every self-recursive local
function) does no reference-count increment or decrement for its captures.
The empty marker is `usize::MAX`, an index no pool slot can have. Native
bytes per level were unchanged in every profile. Against its parent
da19bdb on bygg, `brokkr bench wall` (best of 3), base and change
interleaved as A B B A:

| Workload | da19bdb | Change |
|---|---|---|
| `closure` | 3.1 s, 3.0 s | 2.3 s, 2.3 s |
| `arithmetic` | 2.0 s, 2.0 s | 1.8 s, 1.8 s |
| `method` | 3.3 s, 3.2 s | 2.9 s, 2.9 s |

## Resident-payload GC pacing, 2026-11-17

Measured on bygg with `brokkr bench wall`, using the same pinned workloads
for both arms in A B B A order. A used the Rust sources from `5c2cd0766`.
B used `f285ceedf` plus the GC checkpoint changes. The Rust sources were
swapped in place on main because the historical build required writes to
`.git`. Both arms are stored as `f285ceed-dirty`, with artifact prefixes
`5aebd0c4` for A and `731bfbab` for B. All verdicts and warm opcode costs
agreed across arms.

Each cell is the best of three launches in seconds, followed by its Brokkr
run ID. The change compares the median of the two cells per arm.

| Workload | A1 | B1 | B2 | A2 | Median wall change |
|---|---:|---:|---:|---:|---:|
| `gc_scalar_churn` | 9.279 (66e68d81) | 0.177 (b21458fd) | 0.177 (af8fce56) | 9.035 (becb7e2b) | -98.1% |
| `gc_payload_churn` | 1.630 (91436e36) | 1.524 (4de0b14b) | 1.447 (c54e115f) | 1.687 (d679f06e) | -10.4% |
| `gc_churn` | 2.901 (9d310100) | 2.956 (622e1599) | 2.999 (891b096d) | 2.894 (a83819d2) | +2.8% |
| `fill` | 1.546 (76c57f13) | 1.539 (63b777fb) | 1.528 (53efff3f) | 1.544 (ba7fdfa1) | -0.7% |
| `queue` | 1.458 (86a53e3a) | 1.443 (5e51badb) | 1.442 (0bc0dc82) | 1.454 (8d56e016) | -0.9% |
| `list_literals` | 2.699 (7b74ad21) | 3.115 (682304a3) | 3.132 (bc15163c) | 2.681 (f6d032e7) | +16.1% |
| `rekey` | 1.013 (5a156d6d) | 1.018 (0d8d5026) | 1.020 (4dc75714) | 1.012 (0f2038c6) | +0.7% |

Both pairs agree in direction for every workload. The six launches per
workload and artifact show the within-binary spread. In scalar churn,
the A launches ranged from 9.03 to 12.17 s. Sidecars put most of that
variation in SETUP, which ranged from 3.97 to 7.07 s. WARM took 4.63 to
4.84 s on A and 60 to 61 ms on B. Payload churn also improved in WARM,
from 636-701 ms on A to 558-593 ms on B, with a smaller margin.

List literals slowed by 15.4% and 16.8% in the two pairs. The WARM ranges
were 1.063-1.098 s on A and 1.207-1.240 s on B, with a slowdown in SETUP
as well. These builds measure the whole pacing change together, so they
leave the cause of the list-literal slowdown open, including code layout.
The shifts in `gc_churn`, `fill`, `queue` and `rekey` are small enough
that this campaign leaves them inconclusive.

Follow-up on the list-literal slowdown (bygg, same day). It was already
present at `f285ceedf` (3.1 s against 2.7 s for `5c2cd0766`), so the GC
checkpoints did not cause it. It survived when payload pacing was
neutralized (collection count back to the A count) and when the meter's
add and remove were no-ops. The two builds allocated the same bytes per
function. On perf, cycles went from 14.8G to 17.0G and
`ex_no_retire.load_not_complete` from 5.06G to 7.56G, while L1 misses
fell. The stall sat in the bulk append of `Table::set_list`. In the
metered build, `array.extend(values.inspect(..))` was outlined into a
`spec_extend` that spilled each `Val` through overlapping stack slots and
then reloaded it, a store-forwarding failure. `set_list` now takes a
`&[Val]` and copies with `extend_from_slice`. With that change the
workload measures 2.5 s best of 3 (run `b0083be9`, dirty tree). Stall
cycles in `insert` and the hash push also rose by about 0.5G. They were
left alone because the fixed build beats the baseline.

## Typed numeric loop regions, 2026-11-18

`src/vm/numeric_loop.rs` runs a numeric `for` with a straight-line
arithmetic body as one native loop over an `f64` mirror. Measured on bygg
with `brokkr bench wall`, best of three, A at `341813c` (`--commit`), B the
same tree plus the region (dirty), in A B A order for `arithmetic` and B A
for the controls:

| Workload | A | B | A |
|---|---:|---:|---:|
| `arithmetic` | 2.9 s (2f267db4) | 2.0 s (9991db01) | 2.9 s (82d082e4) |
| `same_obj_read` | | 2.4 s (907cd5eb) | 2.4 s (6599563c) |
| `benchmark` | | 4.1 s (bc2de4e0) | 4.5 s (27446959) |

`same_obj_read` has no qualifying loop (its body reads fields) and does not
move. `benchmark`'s section 1 loop (`sum = sum + i * 2 - 1`) qualifies.
`native_bytes_per_level` is unchanged in dev and release, default and
snapshot: the `FOR_PREP` arm only passes two more arguments to its helper,
and the region runs in an outlined function that is never live across a
native re-entry.

## Invariant field reads and numeric leaves, 2026-11-18

The typed loop region also takes `GET_UPVAL r; GET_FIELD r r k` and
`GET_FIELD` on a register the body never writes, reading each field once at
entry (the region stores, calls and allocates nothing, so the receiver and its
field cannot change). A Lua function whose code up to its first `RETURN` is
the same arithmetic, returning one value, runs inside `call_from_lua` without
an activation when its arguments are numbers (`State::try_numeric_leaf`).
Measured on bygg with `brokkr bench wall`, best of three, A at `528a5b5`
(`--commit`), B the same tree plus both changes (dirty):

| Workload | A | B |
|---|---:|---:|
| `same_obj_read` | 2.4 s (4504964e) | 473 ms (5891aab5) |
| `global` | 6.0 s (c5aa2185) | 2.1 s (4eba9b62) |
| `arithmetic` | 1.8 s (c588fd30) | 1.8 s (cc40b853) |
| `benchmark` | 3.6 s (c5e09438) | 3.6 s (fc82b13b) |
| `method` | 3.0 s (2c3ffdfa) | 2.9 s (524d8734) |
| `rust_boundary` | 3.8 s (cc93a66f) | 3.8 s (05e0cf56) |

`global`'s `add_one` is a leaf; `arithmetic`'s and `benchmark`'s hot calls
are `fib`, which branches and calls, so they are not. `method` and
`rust_boundary` are the controls for the leaf check every Lua-object call now
makes. `native_bytes_per_level` is identical to `528a5b5` in dev and release:
the check is an outlined call from `call_from_lua`.

## Leaf calls inside typed loop regions, 2026-11-18

The typed loop region also takes a callee load (`GET_GLOBAL r k` or
`GET_UPVAL r u`) and its `CALL r` with fixed arguments and one result when
the callee, read once at region entry, has a numeric-leaf plan; the leaf runs
on the region's mirror. Each call still reads the poll flag as its admission
would, and hands the call to canonical dispatch when the flag is set or a leaf
charge could be refused. Measured on bygg with `brokkr bench wall`, best of
three, A at `16e16d5` (`--commit`), B the same tree plus the change (dirty):

| Workload | A | B |
|---|---:|---:|
| `global` | 2.1 s (c6bfd6b6) | 897 ms (7ab71330) |
| `benchmark` | 3.6 s (9175ec9f) | 3.6 s (8da1afdf) |
| `arithmetic` | 1.8 s (47d48688) | 1.9 s (8fb54aae) |
| `same_obj_read` | 473 ms (596b9763) | 489 ms (0c8a61f2) |
| `constants` | 994 ms (2b890054) | 996 ms (a3265f14) |

`arithmetic`'s wall moved without its region moving: a hotpath kernel pair
(f4e7905b, bdf1ca57) puts `run_numeric_loop` at 15.0 ms and 14.8 ms of about
265 ms in both, and the rest is `fib`'s call path, which the change does not
touch, so the delta reads as code layout. Region bodies without calls keep
passes of their own, compiled apart from the call-carrying ones.
`native_bytes_per_level` is unchanged in dev and release, default and
snapshot: the region stays an outlined function that is never live across a
native re-entry.

## Field leaves, 2026-11-18

A numeric leaf may also read and store string-keyed fields of a table
parameter or an upvalue table, and may return nothing: the method shape
`self.count = self.count + n`. Every field site is resolved to a live slot at
entry, and the call runs canonically unless every one is raw present (a
number where it is read) and no stored key is `__index`; the body then reads
and overwrites those slots in order. At this step a loop region called only
leaves with no field and one result (method calls from regions are the
section after next). The candidates weighed were next-opcode fusion of
`GET_FIELD`, arithmetic and `SET_FIELD` in the dispatch loop, and a
persisted fused opcode; both keep the activation and remove two dispatches
of a method call's ten, where the leaf removes the activation, the return
and the body's dispatches together. Measured on bygg with `brokkr bench
wall`, best of three, A at `a77b68f` (`--commit`), B the same tree plus the
change (dirty):

| Workload | A | B |
|---|---:|---:|
| `method` | 3.1 s (a27619f2) | 1.7 s (aee20ab3) |
| `method_chain_counter` | 3.0 s (3f9b9e2d) | 1.6 s (9b7d305d) |
| `method_polymorphic` | 3.5 s (ace7272f) | 2.1 s (c19c6b11) |
| `object_fields` | 3.6 s (51a19861) | 3.4 s (c6d7edd9) |
| `global` | 871, 868 ms (d55d8af7, f7c2495c) | 896, 888 ms (c3def6aa, 41ffa368) |
| `arithmetic` | 1.9 s (4125c38b) | 1.9 s (7fa74398) |

`object_fields` takes only `Unit:damage`; its construction, indexing and the
caller's own field update still dispatch. `global`'s region-called leaf runs
the same code as before apart from an entry check, and its 2 to 3% is
unattributed (no same-binary control was run). `native_bytes_per_level` is
unchanged in dev and release default; release with snapshot moved
`error_tostring` up 16 bytes and the `gsub_pattern_*` rows down 80, which no
changed function is live across (reference/native-calls.md has the table).

## Branches and indexed reads in typed loop regions, 2026-11-18

A region body without calls may also hold forward number compare-branches
and `JMP`s whose targets stay inside the body, and raw element reads
(`GET_UPVAL t u; GET_TABLE t t k`, or `GET_TABLE` on a register the body
never writes) of a table resolved once at entry. Such a body runs through
`State::run_control_region`, which steps its operations by index. Guards come
from a must-written dataflow over the forward edges; a slot first written on
a later pass is published before that pass's checkpoint; an element that is
not a number exits to canonical dispatch at the read's first instruction,
both of which are free. The alternatives weighed were next-opcode fusion of
the pairs in the dispatch loop, which the literal-load and branch-on-true
experiments had already measured flat on this kind of code, and a
whole-function lowered stream, which `cull` does not need: its hot loop
sits inside a `FOR_PREP`. Measured on bygg with `brokkr bench wall`, best of
three, A at `803c19a`, B the same tree plus the change (dirty):

| Workload | A | B |
|---|---:|---:|
| `cull` | 1.28 s (82c1c96e) | 765 ms (15622729) |
| `arithmetic` | 1.9 s (1a3b7dba) | 1.9 s (a3469a0a) |
| `same_obj_read` | 499 ms (52fd535e) | 485 ms (35b0a8fd) |

`cull`'s outer loop holds the inner loop, so it still dispatches; only the
inner pair loop runs as a region. `frame.rs` and every path live across a
native re-entry are untouched (the runner is outlined below
`run_numeric_loop`).

## Method calls and field leaves in typed loop regions, 2026-11-18

A straight-line region may also call a method of a receiver the body never
writes (`GET_UPVAL s u; SELF r s k`, or `SELF` on a register), call field
leaves, and make calls that keep no result. At entry the receiver must be a
table, the method is found through `__index` tables only, with no handler
call and within the metamethod depth, and each field leaf's slots are
resolved; a pass then runs the leaf over those slots with no lookup. The
resolution holds for the whole loop because the leaves store only numbers
into live slots, and the region declines when a store key equals a method
key, a callee's global name or a field the region reads once at entry
(`State::resolve_callees`). The alternatives weighed were next-opcode fusion
of `SELF` and `CALL` in the dispatch loop, which keeps the lookup and the
site resolution on every call and saves two of about six dispatches, and
resolving a varying receiver on every pass, which would make the region the
canonical lookup minus its dispatch. Measured on bygg with `brokkr bench
wall`, best of three, A at `0ab6e1c` (`--commit`), B the same tree plus the
change (dirty):

| Workload | A | B |
|---|---:|---:|
| `method` | 1.6 s (ece37dc2) | 536 ms (b74e3ae0) |
| `method_chain_counter` | 1.5 s (2c3f87cb) | 565 ms (1b5073b8) |
| `method_chain` | 1.2 s (c0681bc7) | 432 ms (c18d1876) |
| `method_chain_wide` | 1.2 s (128c79e2) | 433 ms (99ccaa61) |
| `method_polymorphic` | 2.1 s (807febf3) | 2.2 s (24081cf4) |
| `object_fields` | 3.4 s (d2362146) | 3.5 s (7eddc69a) |
| `global` | 884 ms, best of 5 (52372b22) | 932 ms (c2785e05) |

`method`, `method_chain`, `method_chain_wide` and `method_chain_counter` call
a method of one object per iteration, so under this change they measure the
region call, not a lookup per call. `method_polymorphic` and `object_fields`
read their receiver inside the loop and still look the method up on every
call. `global` runs pure leaves, whose per-call path gained only
compile-time-specialized branches; four rounds of trimming it moved the wall
from 1.1 s to the figure above, and an instrumented kernel pair ran 21 ms on
both binaries, so the remaining 5% is read as layout, unattributed.

## Records in typed loop regions, 2026-11-24

A region without calls may read a table element per iteration as a record
(`local u = us[i]`) and read and store its number fields raw. Each field
site keeps its last slot and reuses it on the next record by key-checked
position (`Table::renew_slot`), which records built in one order share, so
a site costs a key compare rather than a hash lookup; a store resolves its
slot before its `TableWrite` charge and exits to canonical dispatch when
the key is not live. Next-opcode fusion of `GET_FIELD`, arithmetic and
`SET_FIELD` was passed over as for field leaves: it keeps every dispatch of
the loop but two and the per-site lookup. Measured on bygg with `brokkr
bench wall`, best of three, A at `68da812` (`--commit`), B the same tree
plus the change (dirty):

| Workload | A | B |
|---|---:|---:|
| `store_loop` | 4.2 s (492dcd2e) | 1.7 s (ccbd26da) |
| `cull` | 742 ms (ad73271b) | 754 ms (4f3893ac) |

`object_fields` (3.5 s, f7d2db49) calls a method on its record and still
runs canonically. A first build that kept the record arms in the one
control runner put `cull` at 831 ms; compiling the runner apart for bodies
with records brought it back.

## Key-position hints for dynamic string keys, 2026-11-24

A `GET_TABLE` site with a string key keeps the position its key last sat
at, in a per-site cache slot (the D operand), and reads that position
first, checking the live key and a non-nil value; a miss is one keyed
lookup that answers and refills the hint. Records of one constructor share
their slot order, so a sweep such as `us[i][key]` skips the hash probe.
Measured on bygg with `brokkr bench wall`, best of three, A at `0cc99ec`
(clean tree and `--commit`), B the same tree plus the change (dirty):

| Workload | A | B |
|---|---:|---:|
| `column_key` | 5440, 5476, 5464 ms (6f28099c, ba6395c4, 0ac7323f) | 4904, 4905 ms (d692946d, 12a775c1) |
| `rekey` | 997 ms (d64872ad) | 1029, 1023 ms (01411aaa, aa1568d8) |
| `fill` | 1503 ms (5273fec2) | 1546, 1525 ms (2c203726, 013c135a) |

`rekey` and `fill` pay the failed hint check on sites whose key moves or
whose table is fresh. Two probes bound the mechanism: a hint read through
the general `value_if_key` (key/value pair, array-tag check) gave only 5340
ms, and the same string-first branch with no hint gave 5386 ms, so the gain
is the skipped probe read as a value alone. Forcing `Table::get` and
`HashSlots::find` inline into the handler was flat (5475 ms).

## Host field descriptors, 2026-11-18

`State::raw_field` takes a `FieldName` as well as a `&str`. A descriptor
keeps the slot index where its field was last found; a read whose table has
a live key spelling the name at that index skips the name hash, the
string-pool probe and the table probe. The `host_records` workload measures
it: a host callback (`_host_decode`, installed by the bench harness when the
script asks for it) reads eight scalar fields of persistent eight-field,
map-stored records. Measured on bygg with `brokkr bench wall`, best of
three, one dirty library build at `352a11a` with the harness callback
switched between the two key forms, run interleaved:

| Key form | Wall |
|---|---:|
| `&str` | 8.6 s (5663064c), 8.6 s (4ee6763b) |
| `&FieldName` | 7.0 s (b96c3715), 6.7 s (4c0818d1) |

The `&str` path matches the `raw_field` it replaced (8.6 to 8.8 s with the
same callback) only with its lookup marked `#[inline]`; without it the plain
arm ran at 10.0 s. Marking the descriptor lookup `#[inline]` too moved the
descriptor arm to 8.0 s, so it stays out of line. Both are layout effects
of the kind `AGENTS.md` warns about, and the reason those two attributes
are where they are.

## Batch host field reads, 2026-11-18

`State::raw_fields` reads a tuple of fields with one `check_usable` (on
x86-64 an out-of-line MXCSR read for host code) and one table-argument
resolution, where eight `raw_field` calls pay both eight times. Measured on
bygg with `brokkr bench wall --workload host_records`, best of three, dirty
tree at `221a609` with the harness callback switched between eight
`&FieldName` reads and one eight-field batch, run interleaved:

| Callback | Wall |
|---|---:|
| eight `raw_field` | 6.5 s (54193fc1), 6.6 s (1a52d2c2) |
| one `raw_fields` | 4.5 s (a4679e09) |

About 44 ns per decode over about 46 million decodes. The arms are separate
harness builds, so code layout is not excluded, but the gap is well beyond
the layout spread `AGENTS.md` records. The harness callback now uses the
batch.

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
Hosts: plantasjen from 4a722cf on, bygg where the row says so (a different
host and compiler, so its row does not compare with the one above it); the
2026-10-23 rows name x86-64 Linux only.

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
| 912c2d5 with the dependency bumps that followed it | 15856 | 3280 | 3296 |
| host interrupt safepoints on top of that | 16064 | 3296 | 3312 |
| proper tail calls on top of 2a566c8 | 16304 | 3296 | 3312 |
| matcher capture state per search on top of 58e2808 | 16096 | 3168 | 3216 |
| `string.format` literal runs batched on top of d98e154 | 16096 | 3184 | 3216 |
| bygg, `rustc` of 2026-11-05: `__call` resolution out of line on top of ef5f81a | 15904 | 3136 | 3136 |
| bygg, same `rustc`: unchanged string results returned as values, on top of 380266b | 15840 | 3184 | 3184 |
| bygg, same `rustc`: string-replacement `gsub` in one borrow, on top of c4d8fe1 (the table measured 16816 / 3184 / 3200 there) | 16528 | 3040 | 2992 |
| bygg, same `rustc`: library leaves in call dispatch, on top of 5e7a743 | 16576 | 3040 | 2992 |
| bygg, same `rustc`: typed `State::invoke`, on top of b6da85b (b6da85b itself measured 3008 / 3056 in release; dev not re-measured) | 16576 | 3024 | 3072 |
| bygg, same `rustc`: unsized `State`, on top of bea3fdd (bea3fdd itself measured 16576 / 3024 / 3072) | 16800 | 3104 | 3168 |
| bygg, same `rustc`: `gsub` callback results appended in place, on top of 6a6c2b2 (6a6c2b2 itself measured 16880 / 3120 / 3184) | 16528 | 3104 | 3184 |

### 2026-11-18: unsized `State`

`State` gained an unsized tail so host code cannot move it out of the
`&mut State` a callback holds (the soundness reason is at `State`'s type
documentation). Every `&mut State` became a wide pointer whose length,
always zero, is carried through each out-of-line call. Measured against its
parent bea3fdd on the same host and `rustc`, release rows grew 16 to 80 bytes
per cycle (`host_call` 864 to 912, `host_invoke` 896 to 960, the largest
cycle 3024 to 3104), two shrank (`callable_rust` 1488 to 1472,
`sort_comparator` 1424 to 1392), and dev rows grew 48 to 368. Accepted: the
full-depth envelope tests pass on the documented threads. A thin internal
handle (the engine on a sized inner type, re-widened only where host code is
handed `&mut State`) would recover it, at the price of one layout-dependent
unsafe cast or a trait-object public type; not done.

### 2026-11-17: typed host invocation

`State::invoke` (`vm/invoke.rs`) adds one host-api path into
`call_unguarded`'s existing `ArgCount` instance. Its own cycle,
`host_invoke`, is 896 bytes in release against `host_anchor`'s 1024: the
callee is pushed before the arguments, so nothing is inserted and there is
no unguarded wrapper frame. The CLI binary, which never instantiates
`invoke`, still changed: `invoke_rust_fn::<()>` went from 136 to 152 bytes
(`scripts/frame_sizes.py`), a register-allocation change that spills the
callback's returned error pointer, and every release row whose cycle runs a
Rust callback grew 16 bytes with it (`host_call` 848 to 864, `index_rust`
1008 to 1024). The only codegen input that binary sees from the change is
the new `ErrorKind::ResultConversion` variant and its `Display` arm; moving
the variant to the end of the enum did not change the frame. Accepted: the envelope tests pass with it, and the growth is not
in code the change touches.

### 2026-11-18: heap-writing library leaves

`rawset`, `select`, `table.pack`, and raw `table.insert` and `table.remove`
joined the leaves, keeping their arguments in place and running each
callback stack check at the callback's length. Native bytes per level are
unchanged in dev and release, once the new metatable predicate and the
length-taking stack preflight were forced inline (outlined, release rows on
the sort, remove and callable-Rust paths grew 16 bytes). `bench wall`,
bygg, best of 3 against 42ffdee, two interleaved pairs: `heap_leaves` 5.8,
5.7 s -> 4.1, 4.1 s; `sequence_len` 2.5, 2.5 s -> 2.3, 2.3 s; and in the second
pair only, `closure` 2.4 -> 2.3 s and `library_leaves` 3.6 -> 3.7 s, inside
its iteration spread (3.6 to 3.8 s on both sides).

### 2026-11-17: library leaves in call dispatch

`call_from_lua` and `tail_call_from_lua` test the callee's tag inline and
hand a Rust callee to the out-of-line `State::call_leaf` (`vm/leaf.rs`),
which runs the pure library functions without entering a callback. Release
rows are unchanged; every dev row on a path that calls a Rust function from
Lua grew 48 bytes. Moving the tag test out of line too grew dev by only 32
bytes but cost about 3 percent on `closure` (every Lua callee then paid an
extra call), so the tag test stays inline. `bench wall`, bygg, best of 3 against 5e7a743 with the same
workload files: `library_leaves` 6.5, 6.3 s -> 3.5, 3.4 s; `closure` 3.5,
3.5 s -> 3.5, 3.5 s.

### 2026-11-17: string-replacement `gsub` in one borrow

A string or number replacement no longer goes through the per-match
replacement dispatch: `gsub_with_string` runs the whole scan inside one borrow
of the subject, the replacement bytes (and the needle on the plain path), one
meter and the interrupt flag, reads captures off the matcher as the
replacement names them, and interprets the replacement bytes per application
instead of compiling an owned template. Costs are unchanged (no corpus cost
row moved). `bench wall`, bygg, matched pairs against c4d8fe1 with the same
workload files: `gsub_replace` 633, 631 ms -> 470, 470 ms; `patterns` 1.4, 1.3
s -> 1.3, 1.3 s (best of 3 each).

### 2026-11-05: proper tail calls

`OP_TAILCALL` (`State::tail_call_from_lua`, `State::replace_tail_caller`)
added an arm to the dispatch loop and a tail flag to call resolution and the
activation record. Release cycles are unchanged in every row (plantasjen,
`rustc 1.101.0-nightly (e9bbadf68 2026-10-12)`); dev cycles grew 192 bytes
(the `index_*` paths) to 272 (`callable_rust`), 240 for the largest cycle. Three shapes on the way there grew release, each found with
`scripts/frame_sizes.py`:

- The tail-call arm returning a whole `Leave` from its out-of-line helper:
  `Frame::run` reserved a separate return slot, 232 to 264 bytes. The helper
  returns `Result<Option<u8>>` and the arm builds the `Leave`.
- The tail flag as a sixth argument to `begin_callable_call` and a struct of
  two `bool`s for method and tail: LLVM passes such a struct as two
  arguments, so the seventh went on the stack in `call_unguarded`'s frame,
  184 to 200 bytes. `CallShape` packs both flags in one byte.
- A branch building that byte from the method flag made `call_from_lua`
  duplicate its body per flag value; it is built by arithmetic.

The record's chain units and tail mark share one word (`ChainUnits`): as a
`u16` and a `bool` they took two stores per admission.

Wall, interleaved cells of HEAD (`--commit`) against the tree, five pairs,
medians: `tail_call` -2.5%, `method` -1.7%, `closure` -0.8%,
`rust_boundary` -0.7%, `benchmark` -0.4%, `sort` -0.4%, `global` -0.3%,
`arithmetic` +1.5% (B slower in every pair). An A/A control (HEAD in the
main tree against the HEAD worktree) put `arithmetic` at -0.2%, so the
method is unbiased. `arithmetic` runs no tail call. What it does execute
differently is the ordinary call path: a few instructions per call (the tail
flag's subtraction in `admit_lua_call`, the mark mask in `pop_activation`,
building `CallShape`), none of it proportional to depth. Call density argues
strongly against that being the delta: `arithmetic` makes about
8,400 Lua calls per millisecond (26 `fib(20)` per `_bench`, 21,891 calls
each, in about 68 ms), so +1.5% would be about 1.8 ns, roughly 8 cycles, per
call, and `global`, which runs the same call and return path at about 16,000
calls per millisecond, would then lose about 3% unless placement happened to
hide it; it gained 0.3%. The
call-free `constants`, `math` and `fill` workloads were neutral too. A build
with the tail-call arm and its `Leave` variant removed, whose `Frame::run`
diffed identical to HEAD's apart from data addresses, still measured +1.6%,
with `Frame::run` starting at a different offset within its 64-byte line (0
against 32). Placement (of the dispatch loop or of the recursion-heavy
kernel's code) is the best-supported explanation; the evidence is a strong
attribution, not complete isolation. Accepted for the change.

Every scenario's release cycle shrank under the default profile at 0779c08.
The dev figure drifted from 13,552 (41ad774) to 13,680 (0779c08), and every
dev cycle grew by about 2.5 KB between 33734f8 and 912c2d5. Dev builds of
the bench harness on plantasjen place that growth in the dev `Frame::run`
frame: 5512 bytes at 33734f8 and bd9f8d1, 5528 at 9b13569, 7976 at 022a10c
(configurable costs, whose dispatch arms read each charge's weight from the
State and skip a zero weight), 8008 at 912c2d5. Release frames did not move.

### 2026-11-04: interrupt safepoints

Host interrupts (`vm/interrupt.rs`) poll in the dispatch loop, which stays
live across every native re-entry. Release `index_field` per cycle, against
544 bytes without polls:

| Poll placement | Release `index_field` |
|---|---|
| top of the `eval_batched` loop, plus every backward jump | 560 |
| admission (`admit_lua_call`), plus every backward jump | 560 |
| admission (`admit_lua_call`) only | 544 |
| admission, `JMP` and the compare-and-branch and test arms | 544 |
| the above plus `FOR_LOOP` (in its helper or in its arm) | 560 |

The shipped shape polls at admission, in `JMP`, the branch and test arms and
`FOR_LOOP`. Admission refuses backward `FOR_PREP` exits instead of polling
them. (Since then the generic `for` was rotated so that `TFOR_LOOP` is its
back edge and polls like `FOR_LOOP`; the table above predates that and was
not re-measured for it.) `FOR_LOOP` costs 16 bytes per release cycle
wherever its poll sits; it is accepted (2.4 KB at `MAX_NATIVE_CALL_DEPTH`).
Dev grew 160 to 208 bytes per cycle.

Wall cost, 912c2d5 against 3336729 on plantasjen, two interleaved pairs of
`--bench` cells each, medians: arithmetic +1.4%, benchmark +1.7%, closure
+0.6%, method +2.4%, sort +1.4%, patterns +0.5%, the same sign in both pairs
of every workload. Six launches of the 3336729 binary spread 0.5% (method)
and 1.2% (benchmark), and every 912c2d5 cell lies below their minimum, so
the delta belongs to the binary. Its assembly shows no new hot spill: the
cost is the executed poll. LLVM merges every taken jump into one block,
which went from branchless (`cmov`) to a sign branch whose backward side
loads the handle pointer, then the flag, tests and branches; `FOR_LOOP`
continuations and Lua call admissions run the same load, load, test,
branch.

Tried and dropped: a process-wide conservative count of triggered handles
checked before the State's flag, so the fast poll is one load independent
of the State. Method 3791 and 3797 ms against 3817 and 3829 for 3336729,
inside the 3783 to 3802 same-binary spread; arithmetic 3486 and 3465
against 3474 and 3498. The dependent loads were not the cost, and the gate
would have made every State's polls slower while any handle in the process
stayed triggered.

### 2026-11-04: weighted library charges

022a10c (configurable costs) against 9b13569 on plantasjen, two interleaved
pairs of `--bench` cells: patterns 2238 and 2220 ms against 2331 and 2327
(+4.4%), literal_find 4594 and 4577 against 4486 and 4487 (-2.3%), the same
sign in both pairs. The patterns loss is above the same-binary spreads
measured the same day (0.5 to 1.2%). Not the meter's division: the bench
harness sets no budget, and the per-unit `div_ceil` predates 022a10c. The
release harness shows the cause: before 022a10c `MatchState::singlematch`
was inlined into its callers; after it, with the meter's added `Off` variant
in its body, it is an out-of-line call per subject byte. Marking
`singlematch` and `charge` `#[inline(always)]` (after dbda895): patterns
2290 and 2226 ms against 2376 and 2376 for dbda895, back at 9b13569's level.
`match_at`, which recurses natively up to `MAXCCALLS`, grows 152 to 168
bytes; no per-level row moved.

### 2026-11-04: the host store's entry observer

The public `State::set_table` first shared `get_table_unguarded`'s
`call_unguarded::<(ArgCount, &mut bool)>` instance. With two callers LLVM
outlined it (184 bytes) beside `get_table_unguarded` (200), where the read
had inlined it into one 264-byte frame, so every read path through
`get_table_unguarded` grew 128 bytes per release cycle (`host_get_table` 912
to 1040). Giving the store its own observer type (`StoreEntry`) restores one
inlined instance per caller: reads are back at their earlier figures, and the
table library's writes, which used the shared, outlined
`call_unguarded::<ArgCount>` before, shrink (`sort_metamethods_write` 1168 to
1056 in release, 1200 to 1056 with snapshot). Dev writes grow 80 bytes per
cycle.

### 2026-11-17: `__call` resolution out of line

`begin_callable_call` used to resolve a callable object and call the Rust
handler it reached in one frame, which stays live across that callback. It
now calls `State::resolve_callable` (out of line, returned before the
callback runs) and keeps only the invocation: release frames 232 to 56
bytes, the resolver 216. A Rust-valued handler is now called on the receiver
and arguments unmoved instead of being placed and removed again; inside the
old single frame that shortcut alone grew it from 216 to 232 in every shape
tried, the lookup's result slot apparently staying beside the callback's.
The Rust-callee surplus cut in `call_from_lua` lives in its own function for
the same reason (inline, it grew every dev cycle through `call_from_lua` by
16 bytes). `native_bytes_per_level`, host bygg, base ef5f81a: release and
release + snapshot `callable_rust` 1712 to 1552 and `tail_callable_rust` 1040
to 880, every other release row unchanged; dev `callable_rust` 12976 to
12656, `tail_callable_rust` 11264 to 10960, and every dev cycle through a
call from Lua into Rust 16 bytes smaller (`host_call` 10576 to 10560).

### 2026-11-14: the entry observer marked inside `invoke_rust_fn`

Moving the observer's mark from `begin_call` to just after the interrupt poll
in `invoke_rust_fn` (so an interrupt-refused Rust handler is not reported as
entered) passes the observer as a generic argument. `native_bytes_per_level`,
host bygg, base 684ea4b: every release and release-snapshot scenario is
unchanged; every dev and native-call-coverage scenario that reaches a Rust
callback grows 16 bytes per level (`host_get_table` 10896 to 10912 dev).

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
iterator fallback's redundant error truncate did not reduce either figure, so
the eager record and flush before the iterator call stayed at the time.

The idea was retried on bygg against d59e8b8, measured in bytes per scenario
cycle (`native_bytes_per_level`) rather than `Frame::run` frame size: a
`next`/`ipairs` step tried in the `TFOR_CALL` arm before the record and flush,
with the following `TFOR_LOOP` run from the same arm through one outlined
back-edge body. Release, with and without snapshot, kept every row except
`tfor_lua` 720 to 640, `tfor_ipairs` 1120 to 1040, `tfor_host_iterator` 1008
to 928 and `length` 496 to 480. Dev shrank every scenario cycle by 112 bytes
(the three `tfor_` cycles by 208). Two variants were worse, in the same cycle
measure: expanding the back edge twice as a macro cost dev 368 bytes per
cycle, and forcing the fast step inline cost dev 48, while not forcing it cost
release 16; so the step is forced inline only when debug assertions are off.
That shape replaced the eager boundary. In one interleaved A-B-B-A pair of
`wall` cells on bygg, `pairs` went from 1388 and 1385 ms at d59e8b8 to 1301
and 1298 ms for the working tree on d59e8b8 (WARM 543.6 and 541.9 ms to 509.3
and 507.8 ms; no same-binary control run).

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
