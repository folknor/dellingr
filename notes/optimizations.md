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

### bench/method about 2% slower after the slot-token move (2026-11-04)

What: moving inline-cache validation into `Table`'s slot tokens (parent
c969d9f, plantasjen) left `method` about 2% slower than its parent in
matched pairs (+2.2%, +1.7%); `write`, `global` and `method_polymorphic`
moved under that, `same_obj_read` was sign-inconsistent, and native bytes per
level went down or stayed equal in every row. Two shape fixes found on the
way are in the code (one flat global probe; a force-inlined
`Table::slot_is_current` on the SET_FIELD hit), and each recovered a
measured loss. Restoring the old `cached_method_at` body changed nothing;
`resolve_index` and `Frame::run` differ in register allocation and block
order with no added steady-state work found on the path.

Why not yet: no mechanism; the size is in the band code layout alone moves.
Signal: a perf annotate of `resolve_index` or `Frame::run` on `method` that
shows a specific reload or spill the old build did not have.

### patterns about 1% slower with owned-buffer interning (2026-11-24)

What: matched `--commit` rounds on bygg (reference/performance.md, "Owned
results interned without a second copy"): eb33341 against 81090d3 slower in
11 of 12 rounds, +11.5 ms; at fe88e6a, adoption against a borrowed-copy
diagnostic build slower by 15 and 9 ms (8 of 8, 5 of 8). Adoption stays
for `mixed` (2.9% faster). The `sequence_len` slowdown once filed beside
this one did not survive matched arms.

Why not yet: no mechanism. The workload's owned producers are `gsub`,
`reverse` and number `tostring`; hit/miss counts and allocator calls for
them were not taken. Signal: a cost localized to one producer or the intern
path, backed by hit/miss and allocation counts, or a comparison holding code
layout fixed. Wall A/B alone cannot settle it: a one-branch hybrid of the two
behaviours moved `format_literals` 4% past both. Nothing supports a
per-producer opt-out yet.

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

The 2026-11-17 performance hunt (dispatch scope, source-only) proposed the
structural version rather than unchecked indexing: split execution into a hot
register executor and a boundary driver. Enter the executor with validated
code, a frame-relative register window, constant bases and the narrow runtime
pieces it needs; use admission-backed unchecked access inside it, with the
verification capability covering every producer including snapshot loads; run
non-growing ops and direct table fast paths (field probes and global-cache
operations already neither collect nor re-enter) through narrow borrows;
return an action to the driver before stack reshaping, allocation or re-entry;
reacquire the window after. Its point is an enforceable lifetime for the
addressing assumptions, and a home for fused handlers' native-local
intermediates. Risks it named: a stack pointer cannot survive callbacks or
vector growth; a long-lived register slice cannot overlap unrestricted `State`
access; open-result producer/consumer pairs change the stack length mid-pair;
the boundary driver must stay small, since it is live across native re-entry.
It is the "split-borrow design" residue above, argued as half of one engine
redesign with fused operations, not as a standalone bounds-check removal.

Lateral: in every build measured, the dispatch head reloads the code slice's
pointer and length from spill slots (`cmp 0x298(%rsp)` / `mov 0x2e8(%rsp)`)
each instruction. Not shown to be recoverable: keeping them resident competes
for the same registers as handler temporaries.

### Cost accounting on every arithmetic op

Every positive dispatch charge goes through `Frame::accrue` (one rule:
settle at `next >= flush_at`, else keep pending). Generic statically priced
straight-line regions were refused (2026-11-25; reasoning at `Frame::accrue`):
outside typed loop regions, `arithmetic`'s hot `fib` holds one charge per
run between calls. Two unmeasured residues remain:

- By-value batch: `pending` lives on the stack (`add (%rsp)` / `mov
  %rdx,(%rsp)` per charge in release) because its address escapes into the
  outlined helpers. The store/load chain gates the next charge's threshold
  branch, so it is not shown hidden. Threading the batch by value through
  every `local_cost` helper (returning it with errors) needs a matched
  `--commit` A/B on `arithmetic`/`method` and native-stack rows before and
  after. The old "3.6% error-path flush" figure is no estimate: its pair was
  unmatched (live tree against `--commit`), its commit also removed a field
  cache, and the code shape has since moved (one invocation-owned batch,
  settled on every exit).
- Template runs: `Frame::template_init_run`'s pinned writes are the one
  straight-line multi-charge run found (`static_records`: eight writes). A
  read-only prescan summing the run's weights could admit it once when all
  but the last fit strictly under the room, falling back to per-write
  charging; positions stay observable-identical. Unmeasured.

### `GET_UPVAL` indirection

What: canonical `GET_UPVAL` and `SET_UPVAL` index the activation's capture
slice (selected once per `run_frame`), then the pool slot, then branch on
open or closed. Typed regions take number upvalue reads and, without
calls, number upvalue writes (`closed_tally`), so what still dispatches
per instruction is a loop the region declines for another reason
(`closed_mode`, a string-mode test, is the registered workload): stores
other than number fields of a record, non-number branches, calls other
than leaves or invariant-receiver methods, an upvalue write beside any
call, and a captured leaf called on a captured argument, `f(a)` (the
argument's load hoists the callee as a number, so `CALL f` refuses).

Sketch: widen region coverage rather than the canonical arms. `f(a)` needs
the pending loads kept per register (each resolved at first use as a
number or a callee). An upvalue write beside a leaf call could be taken
when the leaf's own captures are proven disjoint from the written cells at
entry (by `UpvalueRef`), with the leaf's capture hoists then still exact.
A closed-cell fast path in the canonical arm was refused (reason at
`State::upvalue_value`). Hoisting the read into a register at compile time
is refused (codegen.rs header: it drops scratch register overwrites that
free dead values).

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
(plantasjen), while `bench/benchmark` (its pre-2026-10-24 workload) is flat
and `--hotpath`
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
codegen fragility a 2026-05-06 attempt at a cross-receiver field-cache
branch met (recorded in e0bef7b's notes text).

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
interleaved `--commit` pairs, `bench/benchmark` 7100/7100 vs 6900/6800 (the
pre-2026-10-24 `benchmark` workload, before it was re-pinned to repeat the
composite kernel) and `bench/constants` 3100/3100 vs 3000/3000, with the
`arithmetic` control level. `--hotpath` on `constants` showed identical per-function shares (the
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

- `Closure` Arc bumps per call: `admitted_frame` clones bytecode and runtime
  from the heap closure (and the capture list, an `Arc` bump only when it
  has two or more captures), plus the `CallInfo` bytecode clone. The
  closure is also looked up twice per call (`lua_function_bytecode` at
  admission, then `admitted_frame`); on `composite/benchmark` at afcd2f0
  (plantasjen) the two lookups' instrumented total matched c2932f8's single
  lookup plus `initialize_frame`, so this is not a measured regression.
  The closure's bytecode is now read through its runtime (one owner), so the
  bytecode clones load one hop further. A route that removes the clones
  without raw pointers (from a 2026-11-17 spar): split dispatch into segments
  that end at every operation that can collect or re-enter, resolve an
  executable id once per segment and borrow its descriptor beside disjoint
  mutable services; a Rust tail callee keeps the id rooted through the
  diagnostic activation. The cost is segment exits and re-resolution at those
  boundaries, so it hinges on how often they occur.
- `CallInfo` still parallels the frame stack (bytecode + ip for tracebacks,
  `print` locations and the cache sweep). Merging needs the traceback to read
  every invocation's frames, including the current frame of each natively
  suspended invocation.
- Generic-for Lua iterators and `__index`/`__newindex` function handlers
  still re-enter natively per call (one `run_invocation` each). Flattening
  them needs a continuation per frame (where the results go). That would also
  take them off `MAX_NATIVE_CALL_DEPTH`.
- The 2026-11-04 defect hunt (`vm-dispatch`) re-raised the first two bullets:
  Lua call admission clones the bytecode for `CallInfo` and the bytecode
  and runtime for the frame. It also clones the capture-list `Arc` when
  the closure has two or more captures. Shorter capture lists are copied
  inline. The references are dropped on return, and the closure in the
  function slot already roots them. Its suggested shape is one stack:
  merge the activation record into the frame, with records owned by
  invocations and tracebacks walking frames. Tail calls now exist
  (`State::tail_call_from_lua`; a tail admission overwrites the caller's
  record in place and `State::replace_tail_frame` swaps the frame): with the
  merge, replacing the caller would be one record instead of a record and a frame,
  and the caller record a Rust tail callee keeps for diagnostics would be its
  frame.

- The 2026-11-17 performance hunt (calls scope, source-only) made both halves
  its lead rewrites. One executor-owned activation stack whose activation holds
  the execution descriptor, cursor, register base, vararg ownership, chain
  accounting and return continuation; diagnostics and GC inspect the same
  stack; an invocation is a stopping boundary within it. Resolve the closure
  once and keep a non-owning execution view, relying on the function slot to
  root it and on the boxed `Closure` for stable storage; separate execution
  and control storage from the mutable VM services so execution can borrow
  immutable descriptors while manipulating records by index. Merging the
  structs but keeping the four `Arc` owners captures only part of it. Main
  risk: a Rust tail callee can collect the retired caller's closure while its
  diagnostic frame still exists (`tests/integration/runtime/tail_calls.rs`
  exercises it), so executable state and diagnostic information need separate
  lifetimes; rooting the old closure longer changes behaviour, and raw pointers
  in place of the `Arc`s are unsafe. The heap hunter reached the same per-call
  clone count from the closure side and proposed State-owned executable
  records addressed by ID, so frames need no owned handles; the host hunter
  counted the same clones on every root invocation and noted the frame stack is
  already pooled.
- Same hunt, continuations: `index_call`, `newindex_call` and `len_call`
  (`registers.rs`), the generic-for Lua iterator fallback (`eval_control.rs`),
  each custom sort comparison (`table_ops.rs`) and `gsub` function
  replacements each run a whole nested invocation lifecycle (native-depth
  accounting, boundary machinery, fresh dispatch, entry-result collection,
  teardown), not an allocation, since `run_invocation` reuses its vectors.
  `IndexOutcome::Call` (`metamethod.rs`) is already the seam. Suspend the
  operation instead: an index continuation knows where the result goes and
  which metamethod units to release; newindex releases units and resumes; an
  iterator continuation projects results into loop registers; a sort
  continuation keeps heap-sort phase, indices and operands. The handler is
  then an ordinary activation on the same stack. Sort must become a resumable
  driver: an adapter still calling `call_unguarded` per comparison keeps the
  cost. Obligations: keep the logical native-depth limit for the eliminated
  re-entry paths (letting formerly failing recursion succeed changes
  script-visible errors); exact comparison order and sort writeback timing;
  metamethod-depth ownership until the operation completes; where errors are
  located and reported; interrupt polls in resumable loops and at admissions.
  Arbitrary host Rust functions still need a real boundary. The codegen and
  table-and-math-libraries hunters raised the iterator and sort halves
  independently. Measured against the claim "the nested invocation is the
  cost" on bygg (de2a001, release): a 2000-element comparator sort, 37746
  comparisons, cost about 1.5 s per probe run where the same number of plain
  Lua-to-Lua calls of the same comparator cost about 1.2 s beyond the loop.
  The re-entry plus all the native heap work is at most ~20 percent of a
  comparison, so the sort half was not pursued (reasoning at `table_sort`);
  measure a handler or iterator the same way before converting it.

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
  renders with a sign or payload (`number_to_text`, `string.format`
  including `%a` and `%q`), NaN is not a valid table key, and saves write
  every NaN as `CANONICAL_NAN_BITS` (`saved_num_bits`). `tostring`/`%p` ids
  for tables, functions and Rust functions come from the counter in
  `format_pointer_ids`, not from handle bits or addresses. `pairs` follows
  insertion order. The snapshot encoder is explicit per variant, so no
  `FORMAT_VERSION` bump; `tests/integration/snapshot/save_golden.rs` compares saves byte for byte
  and is the oracle. No charge depends on representation, so no
  `COST_MODEL_VERSION` bump. `Val` is `pub(crate)`.
- **No host-visible edge:** every exit that hands the host an `f64`
  (`to_number`, `raw_field`, a numeric `ErrorValue`) already returns the
  canonical NaN (`numeral::canonical_nan`), so canonicalizing on entry
  changes nothing the host sees.

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
  in the same slot would read or write the wrong field. Every hit compares the
  cached handle with a live one first, so a dead entry only misses until its
  slot's generation wraps; collection clears such entries every
  `CACHE_PRUNE_INTERVAL` collections (`object.rs`, `State::drop_dead_cache_entries`
  over `RuntimeCaches::drop_dead_entries`), which is safe because only the
  sweep frees, so a slot is reused at most once per collection. Whatever
  replaces the arena must keep that property and its stepping (the
  `version_steps_by_two_per_reuse` test), and a narrower generation must
  bring the interval below half its period: with g generation bits the
  interval must stay under 2^(g-1) collections. Any new side table holding
  weak handles needs the same pruning.
- **Handles.** Objects (`GcHeap`) and the string pool already sit in the
  in-crate generational arena (`object/arena.rs`), whose keys are 64 bits
  (index plus generation, slotmap's encoding) and would have to narrow.
  Keeping a 32-bit index avoids a new limit on
  live objects; a narrower index is a new cap that needs a docs/limits.md
  entry and an error kind.
- **Rust functions.** A `Val::RustFn` is already a 32-bit index into the
  State's function table (`vm/rust_fn.rs`), so it narrows without change.
- **Table storage.** `TableKeyKind::Live(Val)` / `Vacant(usize)` and the
  `Option<Val>` uses need niche encodings, or tables stay 16 bytes per value
  and lose most of the gain.
- **Native stack.** Frames of `Frame::run` and the functions live across
  native re-entry will move; re-measure against the docs/limits.md envelope
  with `brokkr test native_bytes_per_level`.

A cheaper intermediate, if the full version is judged too large, is denser
`Table` storage entries alone; the full version is where the payoff is.

The 2026-11-17 performance hunt (heap scope, its lead rewrite; dispatch scope,
its third) argued for designing compact values, typed object arenas and
collector metadata as one rewrite. Beyond value width: `Val::Obj` names
neither a table nor a closure, so access resolves the slotmap key and examines
`RawObject`, and even `Val::typ` dereferences the heap; `RawObject`
(`object.rs`) reserves table-sized slots while closures live behind a separate
`Box`, so every closure is a large slot plus its own allocation (the table
hunter added that `TableStorage` embeds four full key/value pairs inline, so
every table, and through `RawObject` every closure slot, pays for that
variant). The proposed shape: eight-byte tagged values; distinct table,
closure and string tags; separate typed arenas addressed by compact indices; a
structural split between strong values and weak handles, with generation
metadata only in weak caches, validated before producing a strong value;
allocation and mark metadata in pages with occupancy bitmaps. The collector
part of that landed on its own: the object, string and upvalue pools keep
occupancy and mark bits beside their slots, and the sweep frees
`occupied & !marked` over non-empty words only, so a past allocation peak no
longer raises later collection work. What remains is typed arenas and compact
handles. Both hunters warned against squeezing the 64-bit slotmap key into the
payload or truncating generations (weak-cache and lifetime assumptions), and
against a wrapper over the existing enum. The dispatch hunter suggested
getting unboxed numeric intermediates in fused handlers first, then measuring
what materialized-value cost remains.

---

## Architectural

### Field-site position hints: residue

GET_FIELD and SELF read the site's cached position in any table receiver as
a key-checked hint (`State::get_current_cached_field`, `Table::string_value_at`;
the measurements and the refused shape-id IC are recorded there). What is
left:

- One position per site. A site whose receivers alternate between layouts
  that hold the key at different positions misses on every switch and
  refills from the keyed lookup. N positions per site cover the same hits a
  per-site set of N shape guards would, with no table field; each failed
  guess reads another slot of the receiver, where a shape guard would
  compare an already-loaded id. Promote on a workload with persistent
  alternating-offset misses.
- SET_FIELD keeps the same-table entry only. A cross-receiver store hint
  must prove what `GcHeap::overwrite_live_slot` requires (a live, non-nil
  overwrite of a key other than `__index` through a token valid for the
  current receiver), so it is not the read hint reused. Coverage only: the
  read hint holds a position, never a value, so it is sound without it.
- `bench/polymorphic` and `examples/fields/polymorphic.lua` (2 fields, a
  numeric `for`) now run entirely in a typed loop region, so their header's
  "per-callsite IC never hits" no longer describes them; `wide_sweep` is the
  workload that exercises the dispatch hint.
- The host hunter noted fresh option tables built by hosts get nothing from
  identity caches; the position hint now serves them when they share a
  layout.

### Compiler-recognized field-update fusion

Field updates in method bodies run as field leaves without an activation
(`vm/numeric_loop.rs`), straight-line or branching forward on number
comparisons and field truthiness up to the first `RETURN`; that takes the
activation, every dispatch and (from a region over records) the per-site
lookup misses, more than a fused opcode would. Residue: the same update
inside a function that calls, or returns early, or reads a field only on
a path the receiver lacks it on (every site is resolved on every path, so
such a call declines). There an `OP_FIELD_ADD` (or next-opcode fusion in
the `GET_FIELD` arm) would save two of an adjacent update's three
dispatches; most updates are not adjacent (`self.x = self.x + self.vx * dt`
puts a read and a `MUL` between the read and the add). It owes the
canonical register state and cursor when the add succeeds and the store's
`TableWrite` charge is refused, and must prove the receiver register still
holds the same table at the store.

Signal that would promote it: a workload whose hot non-leaf function
(one that calls) does such updates, with the dispatches showing in
profiling.

### Branching leaves: open costs and declines (2026-11-24)

- The straight-line region runner (`State::run_passes`) declines a loop
  calling a leaf with branches, since it charges whole leaf weights; each
  call still runs as a leaf. Following paths there cost `global` and
  `method` 15-25% in a first build, so it would need its own
  instantiation.
- Every field site is resolved on every path, so a leaf whose skipped path
  reads a field the receiver lacks declines the call.

### Shared record layouts: the storage side

Residue of the compile-time table-shape prediction entry. The constructor
side landed as an execution-side fusion (`Frame::template_init_run`: the
leading run of static loads and pinned writes after a `NEW_TABLE_TEMPLATE`
runs in one call); the batch-fill `DUPTABLE` form, a literal-prefilling
bytecode plan and a compile-time `GET_FIELD_AT` were declined, with the
reasons at `template_init_run` and `State::get_current_cached_field`.

What remains open (2026-11-17 table hunter's lead rewrite): materialize each
constructor's layout once in State-local runtime data, an instance holding a
shape identity and dense values, the shape owning ordered names and the
lookup index; structural mutation converts to dictionary storage carrying its
history. Today every template instance copies the prototype's slot vector and
clones its hashbrown index: `examples/alloc/record_tables.lua` allocates
416 B per eight-field record, all in `reg_new_table_template` (alloc
profile, bygg, 8b07d97), the index one of the two backing allocations.

Why deferred: field reads no longer need it (the key-checked position hint),
so the payoff is allocation and footprint only. Resident bytes, including
the index, pace collection and `gc_score` reports real backing capacity, so
dropping per-instance index bytes is a versioned, changelogged pacing change,
or the accounting must charge a fictional footprint that follows the old
mutation history. A distinct shaped storage variant need not slow ordinary
Map probes, but conversion must keep reserved nil keys, vacant positions and
compaction and reinsertion behaviour, and layout keys must stay rooted by
surviving instances after the creating runtime is gone. Fusing pinned writes
after a dynamic value (`ADD_RK` then `INIT_FIELD_PINNED` in
`record_tables`) is a separate execution-side fusion that needs a witness
that the register still holds the constructed table.

### Share the stdlib install across States

What: today `StateBuilder::build` runs `open_libs` for every new
`State`, building each library table and the global table (`_G`, an
ordinary heap table) from scratch. There is no longer an `Engine` to
hold a prepared copy, so sharing the install needs a new owner: a
prepared template the builder clones from.

Sketch: build the standard environment once into a template and give
new States a copy of it. Watch out: the library tables, `_G` and the
interned key strings all live in one State's heap, so a copy has to
rebuild them in the new heap (or the heap has to support cloning), and
the environment-table record (`GlobalEnv::env_tables`, with each table's
slot policy) is captured right after `open_libs` and must match the copy.

Shipped smaller step on 2026-05-11: stdlib globals and module tables now
install Rust functions through direct `Val`/string-key insertion helpers
instead of pushing key/function pairs through the Lua stack and calling
the public table setters. This reduces the per-State setup work without
changing the ownership model. The process-level `examples/comments.lua`
signal is tiny and noisy on this host (roughly 495us to 487us in a
200-run hyperfine sample), so this does not remove the shared-install idea.

Why deferred: not on the hot path. State construction is amortized
across however long the State lives; even at "one State per request
worker", the install cost is microseconds per request. The bigger
win would be at "one State per request" granularity, which isn't
the recommended embedding model.

Signal that would promote it: a profile showing State construction
dominating request-path latency, or a real "one State per request"
embedder asking. The `host --scenario state_new` bench (construction plus
teardown, about 5 us per State on bygg, 2026-11-24, dirty tree at 8b98de6)
is where a change here is measured.

---

## Stdlib / patterns

### Growing-producer residue after the reusable result buffer (2026-11-25)

`string.format`, `gsub` and `table.concat` now build in the State's
reusable result buffer and decide storage once at the push
(`reference/performance.md`, "Growing producers reuse one result
buffer"). Left open:

- `format_literals` is slower across that change (median 506 to 530 ms,
  0 of 5 faster), and was slower by the same amount across the stack-storage
  change before it, although its executed path is unchanged: allocation and
  per-function call counts on its hotpath file are identical in both arms.
  Read as layout, unconfirmed (no five-launch same-binary control run).
  `patterns` (+0.9%, 1 of 5 faster) and `mixed` (+0.5%, 0 of 5) are inside
  the floor; `mixed`'s counts are identical too.
- A long result that hits an existing string drops its buffer; the
  post-policy probe could hand the unused `Vec` back for reuse (within the
  same capacity cap). A pre-policy hit shortcut would move the collection
  point and is not an option. Unmeasured.

---

## IC extensions (same shape as existing ICs)

### Method IC: three or more receiver metatables at one canonical site

Each allocated GET_FIELD or SELF slot holds two one-level method entries
(two-way LRU, keyed by receiver metatable identity) beside the field, route
and string-method cells; `size_of::<FieldLookupCacheSlot>()` was 184 bytes
at 77954a3, 80 of them the method entries. The SELF-only family split is
refused (reason at `FieldLookupCacheSlot`). The registered method kernels
(`method`, `method_polymorphic`) now run in typed loop regions, whose
per-run cache keys up to eight metatables, so they no longer exercise this
IC; no registered workload has a canonical site cycling three or more
one-level receiver metatables.

The case that would: a canonical site (a host callback `tick(obj)` doing
`obj:step()`, or a shared accessor for a prototype default) cycling three
or more metatables, including per-instance metatables that share one
`__index` table, which exhaust identity-keyed entries though the route is
the same. First shape to try: a hit-only key-checked position pair after the
receiver's raw miss (the remembered `__index` position on the live
metatable still holds `__index`, its value is a table, and the remembered
position there still holds the site's key with a non-nil value; any failed
check falls through), the cross-receiver own-field hint applied twice. If
positions are stable but incompatible, a bounded overflow consulted only
after the inline entries miss. A State-global (metatable, key) backstop is
untested, not refused.

Signal: a workload of that kind with measured replacement misses, or a
memory profile where field cache slots matter.

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

## Host boundary and per-State costs

### Tree expansion during closure tracing

From the spar on the runtime reconcile (2026-11-24): a reached runtime with
nested prototypes could expand its tree into the live-chunk set when closure
tracing first flags it, removing the discovery pass over the runtime map in
`sweep_bytecode_caches`. Pending identities must stay out until tracing
finishes. It moves work into the closure tracer and leaves the literal-root
visit and the map `retain`, so it pays only on states whose retained
runtimes mostly have nested prototypes; no workload has that shape.

### `ipairs` regions: eager record bind, three or more variables (2026-11-24)

A typed `ipairs` region whose body uses the value as a record binds it at
the pass's first operation (`OpKind::Bind`), so a value that is no table
hands the rest of the loop back even on a pass whose path never touches
it (`if i ~= 2 then s = s + u.x end` over `{ {x=1}, false, {x=3} }`), and
the key-1 entry rule means no retry. A guard at the first consuming
operation on each path would take such mixed arrays; it needs a per-path
"bound" fact the solver lacks. No workload holds such an array. Loops with
three or more variables are still declined (the extra ones are nil every
step).

### `numeric_index` cycle residue after the head split (2026-11-24)

The control runner's `IPAIRS` split (`State::run_control_shape`) took
bench/numeric_index from 168.4G to 161.9G retired instructions, under
63e5a4c's 162.7G, but its warm call stayed about 92.0 ms against 63e5a4c's
90.5 (908777f 93; pinned `perf stat` launches on bygg, not a stored bench).
Fewer instructions and more cycles: the hypothesis is placement (the
indirect operation dispatch sits at a different offset in its cache line,
and the `Head` field shifts several plan-field offsets by eight bytes);
no cycle penalty from either has been shown. Unattributed. A
layout-fixed comparison or a cycle-level profile of the two copies would
settle it; more wall A/B will not.

---

## Snapshot path

Benched by the `snapshot` scenarios (`brokkr bench snapshot --scenario
snapshot_save` or `snapshot_load`): a 2000-unit world graph saves in about
2 ms and loads (this build's own save, teardown included) in about 2.8 ms on
bygg (2026-11-24, ms-scale run, dirty tree at 8b98de6). No profile of either
path is recorded yet; nothing here is a candidate until one is.

---

## Micro / take-or-leave

- **Global stores inline an array-part branch they never take:**
  `GlobalEnv::write_cached` is `inline(always)` into SET_GLOBAL and
  SET_BUILTIN and drags in `set_at_index`'s array-part branch, although a
  global's slot is always in the hash part. A hash-only store variant might
  trim `Frame::run` further. Mind the `bench/write` lesson from 2026-10-23:
  removing a never-taken slow branch from this inlined helper made the steady
  state 14% slower, so measure before trusting the idea.

- **Rust-callee `stack.remove(idx)` (B-O5), rejected:** the return-value
  half shipped 2026-07-26 (commit a96cff4). Keeping the callee as frame
  slot -1 to skip the O(args) memmove in `begin_call` was rejected: the
  contiguous Rust callback ABI may use every slot up to the global stack
  cap, so a retained callee would refuse a callback push one slot earlier
  (`rust_call_releases_callee_slot_before_callback_push` in
  tests/integration/host/host_call_preflight.rs). Moving the callee elsewhere still shifts
  the arguments; see the comment at the remove in src/vm/eval.rs.
- **Dispatch micro-items, verify with asm/bench first (B-O6):** opcode space
  is already dense (every opcode number up to the last is assigned), so a
  renumbering (or `#[repr(u8)]` enum with a validated dense range) has
  nothing left to close; LLVM
  likely already emits one jump table with a few dead slots. `get_instr`
  bounds-checks every
  fetch; with one-time validation that all jump targets are in-bounds at
  load/finalize time, the fetch could use a pointer/len cursor - only if
  profiles show it; keep panics over UB.
- **`string.format` cached plan (deferred, 2026-11-24):** `perf record` of
  release `bench/format_literals.lua` (bygg, b85864e) put about half of
  `format`'s own samples (about 10% of the run) on the bytewise scan for the
  next `%`, directive parsing a few percent of `format`, and about 15% of the
  run in allocator growth of the output. The original word-at-a-time scan
  and a bounded capacity hint on the first literal run cut about 25% of the
  cycles in the same profile, and `format`'s
  own time had no hot spot left (wall, bygg, two matched `--commit` pairs in
  both orders against b85864e: 646, 643 to 524, 520 ms; measured with the
  word scan, before `memchr` replaced it unmeasured). Re-argued and refused
  again on 2026-11-25: a plan cannot size `%s` output before its
  `__tostring` runs, so it cannot remove the output buffer, and that buffer
  is now the State's reusable result buffer anyway. A plan cache keyed by template
  identity would now save only the parse and the per-run scans. Reopen only if a
  warm, directive-dense workload with cheap arguments profiles the parse as
  meaningful; `--hotpath kernel` cannot answer it (`format` and
  `format_argument` are native-call inventory entries and carry no
  measurement site), so use `perf`. A plan would also need a byte bound
  on construction (a 16 MiB `%%` template is about eight million escape
  events, and a malformed suffix must not change the error a call raises). A
  cache needs
  invalidation on collection (a `StringPtr` is reused; the pattern cache is
  the precedent), must survive nested `__tostring` calls that can evict it,
  and must keep today's event order (missing argument before parse, parse
  and validation before the charge, charge before rendering, a poll per
  `%`). Residual allocation: `%s` of a non-string without a
  string-returning handler (`TostringRendering::Bytes`).
