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

What: `add_cost!` runs on every costed op: a weight load and zero check, an
add into the local batch and one compare against the flush threshold
`CostState` keeps in `State`. Arithmetic ops cost 1 each, so the bookkeeping
is still a large fraction of their work.

Sketch: charge straight-line runs of fixed-cost ops once per basic block
(the compiler knows the block's static cost), or keep the batch in a
register and check the flush condition only at block boundaries. Must keep
the charged prefix and `cost_used` at `BudgetExceeded` byte-identical
(docs/budget.md, `tests/cost_*`), which is the hard part.

The 2026-11-17 performance hunt (cost-charging scope, source-only) made this
its lead rewrite: statically priced straight-line regions. The ingredients
exist: `Instr::static_units` gives fixed charges and closed `SET_LIST` counts,
the verifier finds control-flow targets, a State's cost model is immutable,
`BytecodeRuntime` is a State-specific bundle shared by a chunk's closures and
frames, and dispatch publishes an authoritative cursor on escape. Price an
execution plan once per State: region boundaries, priced totals and ordered
charge positions for prefix reconstruction. At region entry decide whether all
its positive charges can begin (for charges `c1..cn` and positive remaining
`R`, when `c1 + ... + c(n-1) < R`; the last may cross); if so run handlers with
no per-op meter update or weight lookup and account once at completion, or for
the executed prefix on early exit. An unaffordable region runs with exact
per-charge checks, so charges `3, 5, 2` against budget 4 still execute two ops
and charge 8. Never precharge and subtract (saturation loses information);
keep the entry state and derive the prefix from the cursor on escape. A type
failure in a charged op has already incurred its charge, a blocked charge adds
nothing, an error in a free op keeps earlier charges, a metamethod call
publishes only the prefix before the handler. Potential metamethod calls are
side exits, not region splits. Open `SET_LIST` and native library charges stay
outside fixed totals. It must change the execution structure: a region table
beside `fixed_cost!`, or an "already admitted" flag tested in each handler,
keeps the problem; generate the ordinary and exact-accounting paths from one
source of opcode semantics. `Costing::Off` could then run genuinely
accounting-free. Risks: tiny regions cost more at boundaries than they save,
plan metadata takes cache, generated variants grow code, side exits and source
attribution get harder. It wants codegen to expose region structure and charge
provenance, with canonical saved instructions unchanged. The hunter rated
whole-function or loop-trip precharging a weaker target than straight-line
regions.

### `GET_UPVAL` indirection

What: an upvalue read goes `frame.upvalues` (a `Captures`: inline for one capture, else an `Arc` slice) to a
pool slot, then branches on open (read the stack slot) or closed (read the
stored value). Typed regions read number upvalues, upvalue fields and
upvalue callees once at entry, so `same_obj_read` and `upvalue_bounce` (since
back-to-back number loads are hoisted, 5.1 s to 2.6 s on bygg at 1b783c4)
no longer run them. Loops a typed region declines still read upvalues per
instruction: a `SET_UPVAL` in the body (`queue`'s `tail = tail + 1`), stores
other than number fields of a record, non-number branches, calls other than
leaves or invariant-receiver methods, and a captured leaf called on a
captured argument, `f(a)`: the argument's load hoists the callee as a
number, so `CALL f` refuses. Taking that last shape needs the pending loads
kept per register (each resolved at first use as a number or a callee).

Sketch: no registered workload isolates canonical `GET_UPVAL` dispatch any
more; register one (a declined loop reading closed captures) before
measuring. Candidates are caching the upvalue slice pointer per activation
(as the code slice now is) and a closed-upvalue fast path. Hoisting the read
into a register at compile time is refused (codegen.rs header: it drops
scratch register overwrites that free dead values).

### `SET_UPVAL` clones the pool slot before matching (2026-11-24)

`State::set_upvalue_value` (`vm/registers.rs`) clones the whole `Upvalue`
(tag plus `Val`) out of the pool to decide open or closed, then writes
through `get_mut` on the closed arm: a copy and a second slot lookup per
store. Matching on a borrowed slot (copying only the open stack index)
would drop both. Unmeasured; `queue` (`tail = tail + 1` through an
upvalue) is the registered workload that executes it per iteration. Found
by the UPVAL scout, not part of that item.

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

Key-checked position reuse, measured and refused (fea24a2, bygg,
2026-11-24): on a pointer mismatch, try the site's cached position in the
new receiver (`Table::renew_slot`, which checks the key and a live value)
before the keyed lookup. Built twice: in the outlined read and store paths
(`get_cached_field`, `store_field`), and with the read in the dispatch
hit path (`get_current_cached_field`). Best-of-N wall, HEAD against tree:
outlined method_step 5707 vs 5702 ms, object_fields 3460 vs 3464,
same_obj_read 481.6 both, method_polymorphic 2083 vs 2074, record_tables
2155 and 2160 vs 2231 and 2234; dispatch variant record_tables 2223,
method_step 5744, object_fields 3438. Instrumented, the resolver did get
faster (method_step resolve_index 14 to 11 ns, store_field 21 to 16 ns), but
no release win followed: with `HashSlots` and the forced-inline key hash a
Map probe costs about what the position check plus the cache refill does.
The reasoning stays at `State::get_cached_field`. Typed loop regions keep
the form (`State::record_slot`), where the probe is the whole access.
Untested shape that could still promote it: an own-field sweep over many
prebuilt wide Map records (no construction or call in the kernel) where the
per-table hash indices lose locality. That workload is now registered,
`wide_sweep` (4096 eight-field records from one constructor, four static
field reads per record in a `while` loop so no typed region takes it); a
retry measures there first.

The 2026-11-17 performance hunt raised this from two scopes. The lookup
hunter's lead rewrite: own-field entries hold one `ObjectPtr` and one `SlotRef`
and caches belong to the bytecode runtime, so every entity running the same
method shares one entry, and `update(entities[i])` remembers only the previous
entity; more cached objects only postpone the failure. Make the plan describe
a layout and a slot: guard the receiver's shared layout identity, load or
update the live slot, resolve generically only on a different layout or when
the slot's nil status needs metamethod handling; a small polymorphic cache over
layouts, with a shared lookup structure for megamorphic sites. Table `version`
cannot serve: it starts equal on unrelated tables and ignores ordinary
additions. Risks: layout proliferation, mutation overhead, nil handling (a
reserved nil slot needs a live check unless presence changes the guarded
identity), and cache metadata must stay weak. Its local fallback,
key-checked position reuse, is the measured refusal above. The table hunter's lead rewrite is the storage side:
materialize each constructor's layout once in State-local runtime data, an
instance holding a shape identity and dense values, the shape owning ordered
names and the lookup index; small records hold values inline without repeating
keys; larger ones allocate a value block with no per-instance index;
structural mutation converts to dictionary storage carrying its history; the
table exposes a shape identity and a field-offset token valid across instances.
That removes construction hashing and map population, per-instance key and
index storage, repeated lookup across same-shape objects and repeated tracing
of identical name lists (if shapes are marked once). Its main risk is history:
large templates reserve nil-valued keys while small constructors do not
reserve a key for a nil initializer, deletion and reinsertion change order, and
snapshots distinguish storage kinds and save dead controls and vacant
positions, so conversion must preserve the logical slot history, and shared
names need GC ownership beyond the creating bytecode's literal roots. A general
dynamic shape-transition system is a follow-up. The host hunter noted fresh
option tables built by hosts get nothing from identity caches either.

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

The bytecode keeps only literal ids; the State-local `TemplatePrototype`
(built lazily in `RuntimeCaches`) holds the interned keys, which the bytecode
runtime's literals already root, and each instance copies its slots and
index instead of rehashing the keys.

The 2026-11-17 performance hunt (codegen scope) proposed compiling a
constructor plan holding its key layout, initialization sequence and value
sources, with larger records sharing an immutable key/index structure between
instances and compiler-proven initialization writing known slots directly (the
result is private until the expression completes). Today each instance
copies a per-runtime prototype map holding every key as nil (no per-instance
hashing; the index is still copied per instance rather than shared) and each pinned initialization
(`registers.rs`) checks the constructor table, the indexed key, liveness
bookkeeping and keeps a fallback insert. It must not evaluate every value and
then charge all writes together: a later value may call code or fail, so
evaluation and initialization interleave at today's points. Replay risks: nil
entries and tombstones, pending positional batches in mixed constructors whose
flush timing matters when keys overlap, duplicate-key overwrites, and
snapshots serializing physical slots. Medium confidence in broad payoff,
which depends on the table representation (see the shared-layout account in
"Shape-based polymorphic field IC"). Escape analysis and scalar replacement of
temporary records are follow-ups.

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

### Dev `Frame::run` frame after the weights (+2.4 KB per native level)

022a10c grew the dev `Frame::run` frame from 5528 to 7976 bytes, every dev
native level by the same (`reference/performance.md`, the cycle history).
Release did not move. Presumably each dispatch arm's weight read and zero
check expands into its own stack slots without optimization. Dev still fits
its 4 MiB envelope, with less margin. Worth a look if the dev envelope gets
tight: a shared helper for the fixed charge, or a layout that keeps the
per-arm expansion out of the dev frame.

### Lua-pattern gsub keeps the matcher live across the replacement call

What: `gsub_lua_pattern` calls `append_gsub_replacement` (and through it a
function replacement or a table replacement's `__index` handler) while its
matcher state is still on the native stack, so every level of recursion
through such a callback carries it. The
[Current native bytes per level](../reference/native-calls.md#current-native-bytes-per-level)
table compares the recurring stack usage of Lua-pattern and plain-pattern
function and table replacements.

Sketch: collect the match result (captures as offsets) and drop the
matcher's frame state before invoking the replacement, so the callback
runs from a frame no larger than the plain path's; or move the match loop
into a helper that returns per match and is re-entered.

Why deferred: it fits the envelope with room to spare and nothing is
failing; this buys margin, not correctness. Signal: the envelope tests
tightening, or a reason to raise `MAX_NATIVE_CALL_DEPTH`.

### Short owned string results still allocate in their producer

Interned strings of at most `INLINE_STRING_BYTES` (22) bytes are stored
inline in their arena slot (`StringData` in `vm/object.rs`), so the pool no
longer allocates for them; a short owned result is copied inline and its
`Vec` freed. The producers still build that `Vec` on the heap first
(`concat_values`, `table.concat`, `gsub`, `upper`/`lower`, `reverse`,
`string.format`), so a short miss still pays one allocate-and-free. Open: a
common checked builder that renders short results into a stack buffer (and
long ones into the destination buffer), keeping the allocation-policy check
before the probe. Also left: `table.concat` copies its separator
(`to_bytes(2)?.to_vec()`) every call. Neither is measured; the inline
change itself measured 0.7 to 1.8 percent faster on `mixed`, `patterns`,
`fill`, `format_literals` (two matched `--commit` rounds, bygg; see
`reference/performance.md`), within the layout noise floor, and no
allocator-call counts were taken.

### gmatch iterator residue after the typed iterator

The typed iterator (`GmatchIter`, `reference/performance.md` "Typed gmatch
iterators") now holds its compiled program ("gmatch iterators hold their
compiled program"). Left open:

- A failed search does not record exhaustion, so calling an exhausted-by-miss
  iterator again rescans the remaining suffix and recharges its steps, as
  reference `gmatch_aux` does. Proposal for the user (a cost-model decision,
  not landed): commit the already-admitted exhausted state, `pos = len + 1`
  with `resumed = false`, only on a completed miss (a terminal rejected empty
  match included); interrupts, budget refusals and malformed-pattern errors
  keep leaving the cursor unchanged. No snapshot format change is needed,
  since load already admits that state, but it changes the cost of a repeat
  call after a miss (a `COST_MODEL_VERSION` bump, folded into the unreleased
  version 9 if that is still open) and the saved values of iterators
  exhausted by a miss, both replay-visible.
- The iterator's dispatch arms grew the dev-build native frame of re-entry
  through `begin_call` and `instr_tfor_call` by 96 to 256 bytes a level when
  the typed iterator landed. Outlining `State::gmatch_step_of` and a second
  `invoke_rust_fn` site were tried and made release worse. Untried: one
  borrowed object classification in `begin_call` (and the `__call` chain
  path) that yields either the Lua case or the gmatch step, instead of
  probing the object twice. The dev growth itself is unexplained.
- The pattern cache shares one program per interned key among its owners
  (a weak index of programs evicted while still held), but a number pattern
  coerced to a string has no interned key and still compiles on every call.

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

### Method IC: past two classes per site, and what the second entry costs

Shipped: each field cache slot holds two method entries (two-way LRU by a
victim byte, entries never move, each validated on its own), so a site
serving two one-level classes keeps both. Matched interleaved pairs on
plantasjen, warm ms per call, 7375bb4 against the change on top of it:
`method_polymorphic` 88.9/89.7 to 86.7/86.5 (about -3%; -1.5% to -4.6%
across three builds of the same mechanism, B faster in every pair),
`same_obj_read` -1.2%, `method_chain` +0.5%, `arithmetic` +0.5%.

Open:

- `method` (one class, one level) measured +1.8% (73.7 to 75.2), sign
  consistent within a build but -1.9% on an earlier build of the same
  mechanism. Skipping the victim read on a position-0 hit (a diagnostic,
  not a policy) left it at +1.9%, so nothing on the executed hit path
  explains it; code or data placement is the working hypothesis, not a
  proven one. Revisit if a later layout shift leaves it standing. A
  five-launch same-binary control of `method` on the change (plantasjen,
  2026-10-24) read 75.08, 75.47, 76.91, 75.12 and 75.81 ms, a 2.4% spread,
  so the +1.8% sits inside launch noise and the polymorphic -3% only just
  clears it.
- The slot grew from 88 to 136 bytes, paid by every GET_FIELD and SELF site,
  not only polymorphic method sites. A separate per-slot method vector, or
  method slots only on SELF sites (a compiler cache-family split), would
  stop charging own-field sites.
- Three or more cacheable classes at one site thrash two entries. A third
  or fourth entry is unmeasured: no registered workload has more than two
  one-level classes at a site, and each entry is 40 more bytes per slot.

Signal: a workload with three-plus classes at one method site, or a
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

### `ipairs` regions decline record values and three or more variables

Typed `ipairs` regions (`NumericLoop::recognize_ipairs`) take bodies whose
value is a number or unread. The main game shape, `for _, u in
ipairs(units) do u.x = u.x + u.vx end`, still runs canonically: the step
would define the record, so it would cross the back edge, against the
record rule (a record is defined earlier in the same pass). A candidate
from the spar: a body-entry operation that re-establishes a fresh record
from the yielded stack slot every pass, so no record fact rides the back
edge. Loops with three or more variables are declined (the extra ones are
nil every step). Not measured on a workload with records.

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
  both orders against b85864e: 646, 643 to 524, 520 ms). The scan now uses `memchr`. A plan cache keyed by template
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
  `%`). Residual allocations: float bodies from `number_text`/`format!`,
  `%s` of a non-string without a string-returning handler
  (`TostringRendering::Bytes`), and `%q` of a non-string.
