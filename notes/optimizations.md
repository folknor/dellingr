# optimizations.md

Forward-looking ideas: optimizations considered but not yet implemented, plus
notes on places where current code is deliberately conservative. This is a
working backlog, not a record of decisions. Delete entries as they ship, get
contradicted by new evidence, or stop being worth tracking.

Each entry: what, sketch, why-not-yet, signal that would change the calculus.

Entries tagged like (A-O1) came in through the 2026-07-24 corner audits
(A: front end, B: execution core, C: data plane, D: state/persistence/host,
E: stdlib/patterns), consolidated here after triage against the code on
2026-07-26; the audit's separate candidate file is gone. Bench ratios cited
below are from the 2026-07-27 README table refresh (commit abf7c7b,
plantasjen) unless noted.

---

## Investigate

### Per-instruction weight of the register dispatch loop (2026-10-23)

The entries below share one measurement. perf on plantasjen (Zen 3),
register VM just before the field-read rework: a three-field
`same_obj_read` variant ran about 33 cycles and about 86 machine
instructions per VM op with only 0.14 branch misses per kernel iteration,
so the cost is spread across every op, not mispredicts. The shared dispatch
head was about 40% of `Frame::run` cycles; holding the cursor and base in
locals (`Frame::run_frame`) took arithmetic from 3600 to 3400 ms and
same_obj_read from about 3800 to 3600 (interleaved pairs, plantasjen), so
part of that share was recoverable and part was the sample landing site of
work in the handlers.

Where it stands (2026-10-23): the cursor-in-a-local change (a378895) and
recording call sites only on paths that can call (4794237) shipped; the
bounds-check entry below was measured and is not pursued. What remains is
in the single-digit-percent band that code-layout noise alone moves on these
benches, so the program is parked: pick an entry up only with a workload
where it shows clearly, and treat the NaN-boxed `Val` (under "Full coherent
rewrites") as the larger lever for the same costs.

Each suspect is independent. Work one at a time: perf before and after
(`scripts/perf_hot_lines.py` filters the annotate output), interleaved
`--bench` pairs on arithmetic, same_obj_read and the composite, and
`brokkr test native_bytes_per_level` (dev and release) for anything that
touches `Frame::run`, whose frame is live across every native re-entry.

### Register access bounds checks

Measured 2026-10-23 at eb52657 on plantasjen and not pursued: insufficient
demonstrated payoff. Kept so the experiments are not rerun.

What: every register read and write in the dispatch loop indexes
`state.stack[...]`, a bounds-checked `Vec` access (`cmp len, idx` / `jae
panic_bounds_check`, with the stack pointer and length reloaded from `State`
at each access), although admission reserves `[base, base + max_stack)` and
the verifier bounds every register operand by `max_stack`.

Three throwaway experiments, each against HEAD built by `--commit`, harness
`warm_avg_us`, interleaved `--bench 3` cells (A had a launch spread under 1%
on every workload):

- **Checks removed, pointer still reloaded** (`get_unchecked` on every
  register access in `run_frame`, the `registers.rs` fast paths and the
  numeric-for helpers). Whole-process retired instructions fell 4-8% and
  branches 8-15%, but cycles moved between about -2% and +2%: arithmetic
  -2.0/-2.3%, same_obj_read +0.5/+1.6% (slower, more branch misses), the
  composite +0.3/-0.7%. The checks are predicted and fused; removing them
  alone buys nothing outside the layout band.
- **Activation-wide raw register pointer, unchecked** (held in a local,
  refreshed after every arm whose helper may push). Arithmetic -1.3/-1.7%,
  composite +0.3/-0.7% (cycles +1.1%), same_obj_read -3.6/-6.6% with its
  branch misses halved and a 3-4% launch spread of its own. The field-read
  gain beat every A launch, but its mechanism is unresolved (layout is
  plausible), and its soundness rests on every arm remembering to refresh.
- **The same pointer plus a cached length, checks kept** (the best a safe
  borrowed-slice view could compile to with today's helper boundaries).
  same_obj_read -0.8/-1.8%, arithmetic about -1.5% (one cell), composite
  0.0% (one cell).

None clears a repeatable gain on representative work, and the composite is
flat in all three. Note the composite does not stress repeated reads of one
persistent object; same_obj_read does, and is where the only large delta was.
Untested residue: a split-borrow design whose field-cache hit writes through
the retained register slice (no refresh after a hit). Revisit only with
evidence that persistent-object field reads dominate real callbacks.

The original sketch overpromised: a slice of length `max_stack` does not let
LLVM drop checks on dynamic operands, since the verifier's proof is not
visible to it; only `unsafe` or a per-instruction range proof removes them.

Lateral: in every build measured, the dispatch head reloads the code slice's
pointer and length from spill slots (`cmp 0x298(%rsp)` / `mov 0x2e8(%rsp)`)
each instruction. Not shown to be recoverable: keeping them resident competes
for the same registers as handler temporaries.

### Cost accounting on every arithmetic op

What: `add_cost!` runs on every costed op: a saturating add into the local
batch plus a compare against `COST_CHECK_INTERVAL` and a
`cost_batch_needs_flush` load from `State`. Arithmetic ops cost 1 each, so
the bookkeeping is a large fraction of their work.

Sketch: charge straight-line runs of fixed-cost ops once per basic block
(the compiler knows the block's static cost), or keep the batch in a
register and check the flush condition only at block boundaries. Must keep
the charged prefix and `cost_used` at `BudgetExceeded` byte-identical
(docs/budget.md, `tests/cost_*`), which is the hard part.

### `GET_UPVAL` indirection

What: an upvalue read goes `frame.upvalues` (an `Arc<[UpvalueRef]>`) to a
pool slot, then branches on open (read the stack slot) or closed (read the
stored value). same_obj_read executes one per field read.

Sketch: measure first; candidates are caching the upvalue slice pointer per
activation (as the code slice now is) and a closed-upvalue fast path. A
compiler-side alternative is hoisting a loop-invariant upvalue read into a
register, which is a codegen change with its own aliasing rules (the
upvalue can be written through another closure).

### 16-byte `Val` copies that miss store forwarding

What: a `Val` written as a tag byte plus two 8-byte words (payloads sit at
offsets 1, 4 or 8 depending on the variant) and read back as one 16-byte
vector load stalls on store forwarding. Seen in `ADD_RR` right after a
`GET_FIELD` filled its operand, about 6% of samples on same_obj_read at the
time. `#[repr(C, u8)]` on `Val` (every payload at offset 8) measured within
noise, so layout alone is not the fix.

Sketch: read operands as tag plus payload rather than as a whole `Val`
where the handler only needs the number, or make producers and consumers
use the same widths. The full answer is the 8-byte NaN-boxed `Val` under
"Full coherent rewrites", which removes the split entirely.

### bench/arithmetic +11% wall after compare-and-branch fusion landed (2026-09-03)

What: after A-O7 (commits fd4d333..6cc8493), `bench/arithmetic` measures
4.0s vs 3.6s at pre-fusion 1efb68e in interleaved matched pairs
(plantasjen), while `bench/benchmark` is flat and `--hotpath`
instrumented WARM blocks are consistently *faster* on the fused binary
(15ms vs 17ms per block, -6 to -12%).

Isolation performed: the regression persists byte-for-byte with fusion
emission disabled in the parser (identical bytecode to baseline, only
the dispatch arms present), with one range arm vs six individual arms,
and with `eval_compare` restored self-contained. It tracks the binary,
not the executed path. Conclusion: release code-layout perturbation of
the dispatch loop, hitting the bench where dispatch overhead is the
largest fraction of per-op work (arithmetic is the lightest-op kernel;
its hot `if n < 2` recursion is call+dispatch bound). This is the same
codegen fragility the 2026-05-06 revert under "Shape-based polymorphic
field IC" records.

Five same-binary launches of `bench/arithmetic` at 71afe0e (plantasjen)
all report 4000ms - launch-to-launch spread is below the 100ms reporting
granularity here, so launch noise cannot account for the delta; it is a
property of the binary.

Not chasing it further this pass: the mechanism measurably wins under
instrumentation and on the composite; the loss is layout lottery, and
chasing alignment is open-ended (B-O6's "verify with asm first"
applies). What would settle it: a placebo experiment (rebuild 1efb68e
with an unused-function-sized text shift and re-pair) to characterize
dellingr's own binary-to-binary band, or an asm/alignment look at
`Frame::eval` on this host. The register VM (2026-10-22) rebuilt this
dispatch loop wholesale; the second and third instances below predate
it, so re-measure on the register loop before acting on them.

Second instance (bygg, 2026-10-14): `bench/fill` early WARM_BLOCKs went from
52 ms at b33d4a1 to a flat 54 ms at 67f07aa (+4%, interleaved, stable across
launches), a commit whose changes are all off the fill hot path. `--hotpath`
on both showed identical call counts and unchanged per-function shares, and
the instrumented build's warm average differed by only 1.3%. Same signature
as above: a property of the binary, not of the executed path.

Third instance (plantasjen, 2026-10-22): afcd2f0 against c2932f8 in two
interleaved `--commit` pairs, `bench/benchmark` 7100/7100 vs 6900/6800 and
`bench/constants` 3100/3100 vs 3000/3000, with the `arithmetic` control
level. `--hotpath` on `constants` showed identical per-function shares (the
rest is the dispatch loop itself); on `benchmark` only
`instr_branch_fused` moved (15 to 22 ns, unchanged code), and reading the
compare operands in place, as `eval_float_float` does, did not move the
walls. afcd2f0's own `benchmark` cells spread 7100-7300 across that day.

Fourth instance, on the register loop (plantasjen, 2026-10-23): the commit
that moved call-site recording off the table-op hit paths (measured as
eb034f1, the same code before a notes-only amend) against its parent
6539ac7 in two interleaved `--commit` pairs. Warm averages: `same_obj_read` -2.0/-2.3%,
`method` -3.5/-1.8%, `record_tables` -2.7/-1.8%, `fill` flat, while
`arithmetic` went +1.5/+1.6% (walls 3400 to 3500 both rounds) and the
composite +0.5/+0.5%. The arithmetic kernel executes none of the changed
instructions; `perf annotate` of `Frame::run` shows the same hot sites with
different register assignment and spill slots (the frame shrank 32 bytes),
so it is the same binary-level signature.

### Residual ~2x in `strings/patterns` - decided not to chase (2026-07-25)

What: the 0.2.0-era README recorded `examples/strings/patterns.lua` at 30ms; it
measured 160ms on the same host before the gmatch memoization work and 65ms
after, so roughly 2.4x was recovered and roughly 2x remains unaccounted for.
The bench re-measured at 60ms on 2026-07-27, so the residual is stable, not
drifting.

Decision: not chasing it. The portion with an identified mechanism (per-iteration
subject recopy and pattern recompilation in `gmatch`) is already fixed. What is
left has no suspect attached, and bisecting it is open-ended work against a
bench whose absolute number is now 60ms. Relative deltas remain valid against an
inflated baseline, so this does not block measuring pattern-matcher
optimizations - it would only matter if the residual were a live cost that is
cheap to recover, which nothing currently suggests.

Signal that would reopen it: `strings/patterns` moving again without a
corresponding change to the pattern code, or a pattern-matcher optimization
landing with a much smaller win than its mechanism predicts (which would hint
that something else dominates the bench).

---

## Full coherent rewrites

Large enough to reshape whole subsystems. Each subsumes several smaller
entries below; check the cross-references before starting a subsumed item.

### Call path after the frame-stack flattening (B-O1 remainder)

Lua-to-Lua calls run in one dispatch loop over a per-invocation frame stack
since 27ee714. What B-O1 listed and is still open:

- `Closure` Arc bumps per call: `admitted_frame` clones bytecode, runtime and
  upvalues from the heap closure, plus the `CallInfo` bytecode clone. The
  closure is also looked up twice per call (`lua_function_bytecode` at
  admission, then `admitted_frame`); on `composite/benchmark` at afcd2f0
  (plantasjen) the two lookups' instrumented total matched c2932f8's single
  lookup plus `initialize_frame`, so this is not a measured regression.
- `CallInfo` still parallels the frame stack (bytecode + ip for tracebacks,
  `print` locations and the cache sweep). Merging needs the traceback to read
  every invocation's frames, including the current frame of each natively
  suspended invocation.
- Generic-for Lua iterators and `__index`/`__newindex` function handlers
  still re-enter natively per call (one `run_invocation` each). Flattening
  them needs a continuation per frame (where the results go). That would also
  take them off `MAX_NATIVE_CALL_DEPTH`.

Measurement lesson worth keeping: on plantasjen (Zen 3) the first flattened
version moved frames and records by value and lost 14-22% to store-forwarding
stalls although callgrind instruction counts were within 1-4% of the
recursive path; check `vm/eval.rs` comments before moving a `Frame` or
`CallInfo` whole. Callgrind instruction-level counts plus objdump of the hot
function located those stalls. `perf` works on that host only after
`kernel.perf_event_paranoid` is lowered for the boot (the distribution
default blocks unprivileged sampling); with it, `perf record` cycles plus
`perf annotate` of `Frame::run` attribute stalls to instructions directly.

Native-stack follow-up: `MAX_NATIVE_CALL_DEPTH` is 150, not reference Lua's
200, because 200 did not fit the envelope of the time, 2 MiB in the dev
profile. Since the envelope split it is 4 MiB in dev and 2 MiB in release
(docs/limits.md), which 200 fits on the per-level sizes measured then
(about 11 KB dev, 2.3 KB release, plantasjen). What holds it at 150 now is
demand, not stack: raising it is replay-visible, so it waits for a script
that legitimately nests calls made from Rust past 150.

### 8-byte NaN-boxed `Val` (C-O7)

What: `Val` (`src/vm/lua_val.rs`) is 16 bytes (tag + payload). NaN-boxing
(non-number payloads in the NaN space of an f64) halves stack, table and
upvalue memory traffic, makes `stack: Vec<Val>` copies twice as dense, and
removes the split tag/payload copies that miss store forwarding (see the
16-byte `Val` copies entry above). It is the standard reason reference VMs
beat tagged-enum interpreters on memory-bound workloads.

Why deferred: full-rewrite class. A read-only survey (2026-10-23, after
6539ac7) put it at about 20 files in `src/vm`, heaviest in `table.rs`,
`save_state.rs` and its verifier, `table_ops.rs`, `metamethod.rs`,
`global_env.rs` and the dispatch code; `lua_std` and the compiler do not
touch the VM's `Val` (`codegen.rs` has its own, unrelated one). The plan is
to do it after the release that ships the register VM rather than bundle it
in, because it needs no user migration (below).

Observable surface (the 2026-10-23 survey; re-check before starting):
- **Invisible to scripts, replays, saves and the public API.** NaN never
  renders with a sign or payload (`number_to_string`, `string.format`
  including `%a` and `%q`), NaN is not a valid table key, and saves write
  every NaN as `CANONICAL_NAN_BITS` (`saved_num_bits`). `tostring`/`%p` ids
  for tables, functions and Rust functions come from the counter in
  `format_pointer_ids`, not from handle bits or addresses. `pairs` follows
  insertion order. The snapshot encoder is explicit per variant, so no
  `FORMAT_VERSION` bump; `tests/save_golden.rs` compares saves byte for byte
  and is the oracle. No charge depends on representation, so no
  `COST_MODEL_VERSION` bump. `Val` is `pub(crate)`.
- **The one host-visible edge:** a host that pushes a NaN with a nonzero
  payload would get the canonical NaN back from `to_number`. Not replay
  visible.

Requirements the rewrite must meet:
- **Canonicalize NaN where arbitrary bits enter:** `push_number`, and the
  snapshot decoder (`decode_val`, `build_bytecode_node`), which today loads a
  forged save's NaN payload verbatim. Harmless now; under NaN-boxing such bits
  would decode as a pointer, so this becomes a safety requirement. Arithmetic
  needs no canonicalization if the tag space avoids the two standard quiet
  NaN patterns (`0x7FF8...` and `0xFFF8...`), since Rust arithmetic produces
  only those or passes an input NaN through.
- **Cache aliasing (done).** The inline caches trust a cached handle
  without re-checking the key, so a dead object's entry matching a new object
  in the same slot would read or write the wrong field. Every collection now
  clears the entries naming what it freed (`State::drop_dead_cache_entries`
  over `RuntimeCaches::drop_dead_entries`, right after the heap sweep and
  before any allocation can reuse a slot), so no entry outlives its object and
  a narrower generation does not reopen it. Whatever replaces the arena must
  keep the property it rests on: a freed slot stays vacant until the next
  allocation, and liveness of a handle that was live before the sweep is
  exact until then. Any new side table holding weak handles needs the same
  pruning.
- **Handles.** slotmap keys are 64 bits (index plus generation) and cannot
  produce narrow keys, so objects (`GcHeap`) and the string pool need a
  custom generational arena. Keeping a 32-bit index avoids a new limit on
  live objects; a narrower index is a new cap that needs a docs/limits.md
  entry and an error kind.
- **Rust functions.** They compare and hash by address and are saved by
  registered name. A per-State registry index or the raw pointer both work
  unobservably; the raw pointer relies on user-space code addresses fitting
  about 47 bits (true on x86-64 and aarch64 Linux and macOS today, not a Rust
  guarantee), so it needs an assert or a fallback.
- **Table storage.** `TableKeyKind::Live(Val)` / `Vacant(usize)` and the
  `Option<Val>` uses need niche encodings, or tables stay 16 bytes per value
  and lose most of the gain.
- **Native stack.** Frames of `Frame::run` and the functions live across
  native re-entry will move; re-measure against the docs/limits.md envelope
  with `brokkr test native_bytes_per_level`.

A cheaper intermediate, if the full version is judged too large, is denser
`Table` storage entries alone; the full version is where the payoff is.

---

## Architectural

### Shape-based polymorphic field IC

What: a per-callsite cache for `OP_GET_FIELD` keyed on (table_shape_id,
field_name) -> field_index. Catches the iteration pattern where many
distinct tables with the same logical structure are accessed at one site
(`for _, e in pairs(items) do sum = sum + e.id end`).

Sketch: each Table gets a `shape_id: u64` that updates when keys are
inserted or removed in non-shape-preserving ways. Shape IDs are interned -
two tables built by inserting the same keys in the same order share an ID.
The IC validates by shape_id equality instead of pointer equality.

Why deferred: shape interning is non-trivial. The dominant cost is on
table creation (computing the shape after each insert/remove), and
introducing it everywhere risks regressing table-fill paths for marginal
wins on iteration paths. The current monomorphic IC already catches
same-object access well.

Signal that would promote it: a real workload where polymorphic field
access dominates and `fields/polymorphic` measures 3-5x slower than
`fields/same_obj_read`.

Lighter-weight variant worth weighing first - more honestly described as
a *key-position IC* than a shape IC, since it caches "this key is at
this ordinal IndexMap index" rather than recognizing table shapes.
Validation is a three-state ladder, not a single OR:

1. same table ptr + same `Table::version` - trust the cached index
   (this is what `get_cached_field` already does).
2. same ptr + bumped version, OR different ptr - re-read
   `tbl.get_index(idx)` and accept if the key at that index matches
   the cached key. On accept, refresh the cache.
3. otherwise - slow path.

Note that `Table::version` is bumped specifically when existing
key-to-index bindings may shift: `array_insert`, `array_remove`,
`compact_dead` sweeping tombstones out, and re-inserting a tombstoned
key (which moves it to the back). Deleting a key only tombstones it
(value set to nil, slot kept), and tail-appending inserts and
value-only updates leave indices stable, so none of those bump it.
Cross-receiver validation
therefore *cannot* lean on version equality (different tables can both
have version 0 with completely different layouts); the cross-receiver
correctness guarantee comes from the key-at-index check alone.

What this catches: different tables that happen to lay out the same key
at the same ordinal (e.g. all `{id=…, value=…}` records constructed in
the same key order). What it doesn't catch: two valid layouts with the
same logical fields in different orders (`{id=…, value=…}` vs
`{value=…, id=…}` will thrash). If real workloads turn out polymorphic
across multiple layouts, a small N-way PIC is the next step rather than
a single-slot key-position cache.

How this might compare to the heavier shape-IC design depends heavily
on workload mix and isn't yet measured. One hypothesis worth testing:
in production scripts the bigger win comes from compile-time
record-shape prediction (see "Compile-time table-shape prediction"
below), which would make the runtime IC variant less load-bearing -
but this is unsupported by dellingr's own benches today.

Tried in 2026-05-06, reverted: dropping the early-exit on ptr mismatch
in `get_cached_field` and falling through to key-at-index validation
regressed both `same_obj_read` (+11% wall time) and `polymorphic` (+9%)
even with the cross-receiver branch extracted to a `#[cold]`
non-inlined fn. Two reasons it didn't pan out as written:

1. Polymorphic record-shaped tables (`{id=…, value=…}`, ≤4 fields)
   stay in `TableStorage::Inline`, whose slow path is already a
   1-2-entry pointer-compare scan. Cross-receiver validation
   (heap-deref + `get_index` + key cmp + `cache.set_field` write)
   is more work than that, so it loses on Inline.
2. Even with `#[cold]` extraction of the cross-receiver branch, the
   extra control-flow edge in `get_cached_field` shifts LLVM's
   inlining/register-allocation choices in the bytecode dispatch loop
   enough to slow the same-ptr fast path. Couldn't get the hot path
   to compile bit-exact-equal to the original.

Promotion would need either (a) a real workload where polymorphic
record-shape access is on Map-storage tables (>4 fields), where the
slow path's hash lookup is expensive enough to amortize the extra
work, or (b) a way to add the cross-receiver path without
perturbing the same-ptr branch's codegen.

Both conditions may now hold (2026-10-23). For (b): since f92638c the
register VM's `reg_get_field` inlines only the same-table hit, and every
miss goes to the outlined `State::resolve_index`, so a key-at-index check
placed there, before the hash lookup, does not touch the hit path's
codegen. For (a): `bench/record_tables` builds a fresh 8-field (Map)
table per iteration and reads every field, so each read misses the
identity cache and hashes; at a378895 (plantasjen, before the hash
function was forced inline) `hash_table_key_value` and
`IndexMap::get_index_of` were about 31% of its cycles, construction-side
inserts included. Validation must read the live key at that slot in the current
table (`get_index`, not the value-only `get_index_value`), treat a dead
or nil slot as unanswered, and keep the metatable fallback. Restricting
it to Map storage sidesteps reason 1.

### Compiler-recognized field-update fusion

What: parse `t.x = t.x + n` into a single `OP_FIELD_ADD` that does
GET + ADD + SET in one instruction, eliminating dispatch overhead and
ideally sharing one cache slot across the read and write of the same
(receiver, key) pair.

Why deferred: requires parser-side pattern recognition, a new opcode, and
unifying cache slot management. The win compounds (one dispatch + one
cache lookup instead of three), but the complexity is parser-level, not
VM-level, which is where we've been working. With register bytecode it is
a codegen peephole over `GET_FIELD`, an arithmetic op and `SET_FIELD` on the
same receiver register and name.

Signal that would promote it: a workload where method bodies of the form
`self.field = self.field + ...` dominate, and the per-instruction
dispatch cost shows up in profiling.

### Compile-time table-shape prediction

This is two distinct work items that share a name; they have very
different ROIs and should be considered separately.

**(a) Pinned-shape constructor template / direct record build.**

Shipped first slice on 2026-05-11: table constructors with more than
four statically visible fields now emit `NewTablePresized(n)`, which
allocates map-backed storage at the final constructor size instead of
starting inline and promoting after the fourth insert. This is not the
full record-build design below, but it validates that constructor
allocation growth is worth caring about:
`examples/alloc/record_tables.lua` moved from 345.6ms to 277.9ms
against the pre-change binary on this host (~1.24x).

Shipped second slice on 2026-05-11: pure named-field constructors with
more than four unique keys now store a per-Chunk template of
interned-string ids, allocate map storage with those keys pre-installed,
and initialize values by pinned entry index. This avoids the repeated
key lookup on each value write, while falling back to `NewTablePresized`
for computed keys, array entries, vararg/spread entries, and duplicate
named keys. The measured win over capacity-only was small and noisy on
this host: `examples/alloc/record_tables.lua` moved from 264.4ms to
258.4ms in the cleaner run (~1.02x), with an earlier run at ~1.05x.
`examples/alloc/short_tables.lua` was effectively flat.

Remaining idea: a stronger `DUPTABLE`-style opcode would batch-fill a
pure named-field constructor at the end instead of preinstalling nil
placeholders and then overwriting by index. That would remove the
per-field bytecode dispatch and avoid the placeholder writes, while a
single table-build helper could replay normal `insert` semantics in
source order for nil values and duplicate keys.

Why deferred: the current parser emits each field write immediately.
Batch-filling means either delaying named-field writes while preserving
fallback behavior for mixed constructors, or adding a more general
builder instruction for values accumulated above the table on the VM
stack. That is a larger bytecode/stack-shape change than the pinned
write path, and the pinned path only proved a small remaining win after
capacity pre-sizing.

Caveat: if the template were to hold `Val::Str` or table objects
directly, closure marking / chunk rooting would need extension to keep
those reachable. Storing only literal-id ints in the template avoids
this entirely - the strings get interned at template-instantiation
time using the existing literal-string path.

**(b) Local-tracked shape inheritance (rewrite-the-analysis).**

What: callsites that read fields on a local known (statically) to hold
a pinned-shape table can skip the field IC's warmup and emit
`OP_GET_FIELD_AT(reg, key, expected_idx)` that goes directly to
`get_index(idx)` with a key-validate. Same machinery the integer-const
opcode would need for its fast path - they should share a sketch.

Why deferred (separately from a): dellingr's parser has no SSA, no
data-flow scaffolding, and no local-aliasing analysis. Tracking shape
inheritance "conservatively across reassignment, control flow, and
function boundaries" is build-the-analysis-from-scratch, not
add-a-pass. Reassignment, branch joins, and any escape into a
RustFunc/Lua-call/closure-capture/table-field-store would each give
up. An analysis that gives up frequently leaves most of the win on
the table.

Signal that would promote either: profiling showing first-iteration
miss cost dominates after the runtime field IC is sufficient for
steady state - short-loop workloads where the IC never warms up. (a)
is also worthwhile independently if benchmarks show construction cost
on record literals dominating their first-use cost - measure before
committing.

Whether the headline runtime-IC wins production game scripts more or
less than constructor pre-sizing is unmeasured for dellingr; the two
are complementary (cold/first-touch vs. warm/steady-state), not
substitutes.

### Move stdlib install from per-State to per-Engine

What: today `State::with_callbacks` -> `open_libs` re-runs the
standard library install on every new `State`, allocating each
RustFn entry into the `globals: IndexMap<String, Val>`. With the
`Engine` layer in place, the stdlib install can happen once on
`Engine::new` and `Engine::new_state` can clone the prepared
globals into the new State.

Sketch: `Engine` carries a frozen `stdlib_globals: IndexMap<String,
Val>` and a frozen `stdlib_builtins: [Val; Builtin::COUNT]` populated
once at construction. `new_state` clones both into the State.
Watch out: stdlib `Val`s currently include `Val::Str(StringPtr)` for
some entries, which are tied to one specific `StringPool`. Either
the stdlib stops baking string pointers (use `&'static [u8]` and
intern lazily on first use), or the Engine carries a shared
string pool that new States inherit.

Shipped smaller step on 2026-05-11: stdlib globals and module tables now
install Rust functions through direct `Val`/string-key insertion helpers
instead of pushing key/function pairs through the Lua stack and calling
the public table setters. This reduces the per-State setup work without
changing the ownership model. The process-level `examples/comments.lua`
signal is tiny and noisy on this host (roughly 495us to 487us in a
200-run hyperfine sample), so this does not remove the per-Engine idea.

Why deferred: not on the hot path. State construction is amortized
across however long the State lives; even at "one State per request
worker", the install cost is microseconds per request. The bigger
win would be at "one State per request" granularity, which isn't
the recommended embedding model.

Signal that would promote it: a profile showing State construction
dominating request-path latency, or a real "one State per request"
embedder asking.

---

## Stdlib / patterns

### Lua-pattern gsub keeps the matcher live across the replacement call

What: `gsub_lua_pattern` calls `append_gsub_replacement` (and through it a
function replacement or a table replacement's `__index` handler) while its
matcher state is still on the native stack, so every level of recursion
through such a callback carries it. `native_bytes_per_level` on plantasjen
when the envelope tests were widened: Lua-pattern gsub with a function
replacement 14480 bytes per level in dev and about 3.7 KB in release, with
a table replacement 15456 and 3872, against 11072 and 2080 for the plain
pattern path. It is the heaviest native call path, so it sets the dev
envelope's margin (about 2.3 MB of 4 MiB at `MAX_NATIVE_CALL_DEPTH`).

Sketch: collect the match result (captures as offsets) and drop the
matcher's frame state before invoking the replacement, so the callback
runs from a frame no larger than the plain path's; or move the match loop
into a helper that returns per match and is re-entered.

Why deferred: it fits the envelope with room to spare and nothing is
failing; this buys margin, not correctness. Signal: the envelope tests
tightening, or a reason to raise `MAX_NATIVE_CALL_DEPTH`.

### gmatch: drop the Lua-side wrapper and table-backed iterator state (E-O2)

Partially shipped as of 2026-07-25 - the per-iteration subject/pattern
copies and pattern re-validation are gone (`memoize_gmatch_pattern`,
`gmatch_subject_matcher_and_cost_meter`); that was the bulk of the win
(`strings/patterns` 160ms -> 60ms).

What remains (`string.rs`): one Lua call into the compiled wrapper chunk
(`gmatch_wrapper()`, still a compiled Bytecode) + one RustFn call, plus
`get_table` lookups ("s", "pos") and one `set_table_raw` against the
table-backed iterator state, every iteration.

Explored 2026-09-03, and the original sketch understates the cost. The
generic-for protocol does pass `(state, control)` to the iterator, so a bare
RustFn works *inside a for loop* - but reference Lua also supports capturing
the iterator and calling it standalone (`local it = s:gmatch(p); it()`),
where it receives no arguments at all. The state therefore has to live
inside the callable, and dellingr's `RustFunc` is a plain fn pointer by
design (determinism, snapshot registry keyed by address). Dropping the
wrapper needs a new captured-state Rust callable - a `Val` variant or heap
object holding (fn, state) - which touches GC marking, Val size (and the
NaN-boxing plan), and snapshot encoding. That is rewrite-class, not a
stdlib cleanup; only worth doing as part of a broader Val/callable rework,
or if `strings/patterns` profiling shows the wrapper call dominating.

### Keep the compiled-pattern cache across collections

What: `State.patterns` (compiled patterns keyed by the pattern string's
`StringPtr`) is cleared at the top of every collection, because sweep may
free a key string and a later allocation may reuse its pointer. A script that
allocates while it matches therefore recompiles its patterns after every
collection. Instead, prune only the entries whose key string was not marked
(weak keys, decided after marking and before sweep), or key the cache by the
pattern bytes so pointer reuse cannot alias.

Why deferred: unmeasured. No registered workload matches while allocating
enough to collect often; `strings/patterns` and `alloc/gc_churn` each cover
only one half. Write that workload first and check how much compile time a
collection actually costs before changing the cache.

---

## IC extensions (same shape as existing ICs)

### String method calls take one outlined call

What: `s:upper()` and every other method call on a string receiver is a
`SELF` whose receiver is not a heap object, so `reg_get_field`'s inline
own-field hit cannot answer and the op calls the outlined
`State::resolve_index`, whose string step answers from the string-method
cache. Since the register-native rewrite on top of df80d81 there is no
stack round trip, call-site record or reentrant frame on that path; what
is left is the call itself and the resolver's prologue.

Sketch: check the string-method cache inline in `reg_get_field`, after
the own-field hit, with the validation `get_cached_string_method` does
(string metatable `__index` slot and library table version). Mind
`Frame::run`'s frame: `reg_get_field` inlines into it, and it is live
across every native re-entry, so measure `native_bytes_per_level` too.

Measure with `bench/patterns`, whose kernel calls string methods in its hot
loop (`self:match`, `:reverse()`, `:gsub()`, and a `:trim()` added to the
string library), though its time is dominated by the matching itself; the
rewrite moved it about 1.7% there (plantasjen, df80d81 against the
rewrite), within the unrelated control's shift.

### Table-library fallback IC for the hit path (merged with B-O4/C-O5)

The miss-path half of this entry shipped 2026-07-26 (commit 11854a8): a
pristine-shape gate on `push_table_library_field` returns nil for
non-member keys without probing the library, and `bench/miss` moved 5.2s
to ~3.0s (worktree-matched interleaved pairs, plantasjen) with
`same_obj_read` and `cost_used` unmoved. What remains is the hit side:

What: when `tbl:insert(...)` etc. do fall through to the `table` library,
cache the resolved value at the call site - same shape as the shipped
string-method IC. The shipped gate's cached member-name pointers and
shape guard are reusable ingredients.

Why deferred: rarer pattern than string methods, and the fallback hit
path isn't a hot bench; the miss gate already removed the common cost
without any per-site cache.

Signal: a workload where `t:insert(...)`-style sugar fires in a tight
loop.

### Method IC: one entry per site ping-pongs between classes

What: the method IC holds one entry per site, and every one-level hit
overwrites it. A `shape:area()` site serving two one-level classes in
alternation (`bench/method_polymorphic.lua` has Circle and Square) misses
its entry on every other call and redoes the direct lookup. A two-entry or
small polymorphic entry would keep both.

Why deferred: unmeasured in isolation. The 2026-10-23 sticky-miss removal
already made that workload 10% faster (plantasjen, against 4a722cf); the
remaining per-call cost is a few table reads.

Signal: `--hotpath` on `examples/calls/method_polymorphic.lua` showing
`State::resolve_index` hot. The register-native rewrite on top of df80d81
made that workload about 8% faster again (plantasjen, matched pairs).

Note: caching misses was tried and dropped. An exact absence proof (no hash
slot for the key, unchanged version and slot count) is sound but validating
it reads as many tables as the uncached miss, so it saved nothing on
`bench/method_chain.lua`. The repeated direct attempt that cost it its time
is gone since the register-native rewrite (each table read once per
lookup; 85.3 to 64.7 ms warm, plantasjen, df80d81 against the rewrite).

### GET_FIELD slow-path cache repopulation after `__index` resolution

What: after `State::resolve_index` resolves through `__index`, the resolved value lives somewhere
identifiable (the index table for the table-handler case). The method IC
already caches this. The direct-field cache could also populate when the
resolution happened to bottom out in a raw entry on a relevant table,
even though that entry wasn't on the receiver itself.

Why deferred: substantial overlap with the method IC, which already
handles the OOP case. The remaining cases (multi-level `__index` chains
without metatables in the middle) are uncommon.

---

## Compiler-side ideas

### GET/SET cache slot sharing

What: when the compiler can prove a `OP_GET_FIELD` and `OP_SET_FIELD`
refer to the same (receiver, key), assign them the same cache slot.
Saves a slot per such pair and shares warmup state.

Sketch: parser-time def-use analysis. For `self.count = self.count + 1`,
both the GET and SET refer to `self`'s `count`. They could share an
entry shape (table_ptr, version, index) since both lookups bottom out
in the same table at the same index.

Why deferred: requires DEF-USE analysis that the parser doesn't currently
do. The memory savings are small (one cache slot per pair = ~24 bytes per
shared site). The warmup-sharing benefit is one fewer slow-path call per
new pair, which is bounded.

Signal: profiling showing the parser is fast and we have headroom to add
analysis passes, OR a workload where many distinct (receiver, key) pairs
each get fewer than ~3 accesses (so warmup amortization matters).

---

## Snapshot path

Still unmeasured as a whole: snapshots are driven from Rust, not from Lua,
so benching them needs harness work rather than a new `.lua` file (tracked
in todo.md's workload-registry entry).

---

## Micro / take-or-leave

- **Global stores inline an array-part branch they never take:**
  `GlobalEnv::write_cached` is `inline(always)` into SET_GLOBAL and
  SET_BUILTIN and drags in `set_at_index`'s array-part branch, although a
  global's slot is always in the hash part. A hash-only store variant might
  trim `Frame::run` further. Mind the `bench/write` lesson from 2026-10-23:
  removing a never-taken slow branch from this inlined helper made the steady
  state 14% slower, so measure before trusting the idea.
- **Keyed global writes invalidate every builtin mirror entry:**
  `GlobalEnv::write` (the uncached keyed path) reaches `contents_mut` ->
  `get_mut`, which moves the watched epoch even when the key is not a
  builtin. Correct but broad: every uncached or new global write forces each
  builtin read to revalidate. Matters only if a workload interleaves new
  global definitions with builtin-heavy code.

- **Error-path cost flush costs ~3.6% on `arithmetic`:** charging pending
  batched cost on runtime errors moved the dispatch loop into an inner
  `eval_batched` (`src/vm/frame.rs`) that takes `local_cost` as `&mut u64`.
  Two interleaved `--bench` pairs on bygg, e475e51 (before) vs 8d3151a
  (after): 2800 / 2900 ms both times, sign-consistent. Not yet separated
  from layout: run the five-launch same-binary control, then try keeping
  `local_cost` a by-value local inside the loop and returning it alongside
  the error (e.g. `Err((e, pending))` from the inner loop) so it can stay in
  a register.

- **Rust-callee `stack.remove(idx)` (B-O5), rejected:** the return-value
  half shipped 2026-07-26 (commit a96cff4). Keeping the callee as frame
  slot -1 to skip the O(args) memmove in `begin_call` was rejected: the
  contiguous Rust callback ABI may use every slot up to the global stack
  cap, so a retained callee would refuse a callback push one slot earlier
  (`rust_call_releases_callee_slot_before_callback_push` in
  tests/host_call_preflight.rs). Moving the callee elsewhere still shifts
  the arguments; see the comment at the remove in src/vm/eval.rs.
- **Dispatch micro-items, verify with asm/bench first (B-O6):** opcode space
  is 0-26, 30-55, 60-72 - only two small holes (27-29, 56-59) since the
  fused branches filled 64-69, so a dense renumbering (or `#[repr(u8)]`
  enum with a validated dense range) has little left to close; LLVM
  likely already emits one jump table with a few dead slots. `get_instr`
  bounds-checks every
  fetch; with one-time validation that all jump targets are in-bounds at
  load/finalize time, the fetch could use a pointer/len cursor - only if
  profiles show it; keep panics over UB.
- **Table micro (C-O8):** remaining note only -
  `try_insert_table_direct`'s double probe is only on the
  metatable-present path; the no-metatable path is single-probe
  already - fine as is. (The `get_full` single-probe and the
  promote-to-8 capacity shipped 2026-09-03.)
- **Stdlib micro (E):** `string.format` should format into one output buffer
  instead of per-directive Vec round-trips (invasive across ~15 helper
  functions for a micro win; measure before bothering).
  The plain-pattern classifier treats `-` as magic even though a leading `-`
  with no preceding class item is literal; conservative is fine, just noting
  the fast path misses needles like `"-foo"`.
- **String-pool index shape:** `StringPool` (`src/vm/object.rs`) computes
  an Fx hash and then `IndexMap<u64, Vec<StringKey>>` hashes that u64 again
  (now with foldhash). A `hashbrown::HashTable<StringKey>` keyed by the
  stored hash drops the second hash and the per-bucket `Vec`. Measure on
  `alloc/gc_churn` and `strings/mixed`.
