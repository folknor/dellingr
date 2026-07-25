# Bug hunt 2026-07-24 - consolidated optimization candidates

Consolidated and deduplicated from the same five corner audits as
[bugs.md](bugs.md) (A: front end, B: execution core, C: data plane,
D: state/persistence/host, E: stdlib/patterns). Nothing here is benchmarked
or vetted - these are the auditors' proposals, kept separate from the
tracked backlog in /OPTIMIZATIONS.md until triaged. Auditors were instructed
not to re-propose already-tracked items; where a proposal extends or folds
into a tracked entry, that is noted. The audit ground rules assumed pre-1.0
(internal API breakage acceptable, aggressive rewrites on the table), with
determinism, per-opcode cost accounting, and the "Won't implement" list as
hard constraints - every item below claims to preserve those.

Cross-references like "#N" without a file refer to bugs.md finding numbers.

---

## Measured baselines (2026-07-25)

Six diagnostic probes were added to `examples/` so that the candidates below
have something to measure against; before this the bench suite covered
steady-state execution well and was blind to parse time, miss paths, global
writes, literal interning, constant folding and GC marking. Full numbers are in
the README table; what they say about the candidates here:

- **#8 (per-call literal interning) is the standout.** `calls/many_literals`
  runs at 574ms / 42.9x lua5.5 - an order of magnitude worse than any other
  bench. It is a designed worst case (32 constants, returns from the first
  branch), so it is not a claim about typical code, but it does establish that
  the cost is real and scales exactly as the item predicts.
- **The parse-time cluster (#6, #7, #22, #23) is worth doing.**
  `parse/large_source` is 8.0x lua5.5 / 7.3x lua5.2 on a 5000-line chunk, the
  worst ratio outside the literal probe. Note this is a single measurement, not
  a demonstrated quadratic; confirming the curve needs a second file size.
- **#10 (table-lib fallback on the miss path) is confirmed.** `fields/miss` is
  6.6x lua5.5 against `fields/same_obj_read` at 3.9x, so a missing field costs
  roughly 1.7x what a hit does in relative terms.
- **#9 (SET_GLOBAL IC) is confirmed.** `globals/write` at 5.6x lua5.5.
- **#17 (constant folding) is confirmed.** `numerics/constants` at 6.7x lua5.5
  against `numerics/arithmetic` at 4.4x; the pair isolates folding because
  arithmetic.lua deliberately has no foldable expression.
- **#5's GC half and #21 look unurgent.** `alloc/gc_churn` is 1.6x lua5.5, the
  best ratio in the suite - marking is already competitive, so the case for the
  iterative mark walk rests on the robustness argument (removing the unbounded
  recursion) rather than on throughput.

Still unmeasured: the snapshot path (#26, and the save-walker half of #5).
Snapshots are driven from Rust, not from Lua, so benching them needs harness
work rather than a new `.lua` file.

---

## Full coherent rewrites

### 1. Register-based codegen (A-O1; biggest throughput lever)

The parser emits a pure stack machine: `x = a + b` is GET_LOCAL, GET_LOCAL,
ADD, SET_LOCAL - four dispatches plus stack traffic; a register VM does it
in one ADD(dst, a, b). Reference Lua's 5.0 move from stack to register VM is
the single largest reason for the persistent 1.6-3x gap on arithmetic/field
benches (`numerics/arithmetic` 3.2x, `fields/same_obj_read` 2.4x vs lua5.2).
The 32-bit ABC encoding in `instr.rs` already has the operand room; locals
already live in fixed frame slots, which are registers in all but name. The
expression parser would grow a lua-style expdesc with delayed emission (the
existing `ExpDesc`/`PlaceExp` split is halfway there), and
`mark_call_base` / `dup` / `swap` gymnastics disappear.

Hard-constraint interactions: determinism unaffected; per-opcode cost
accounting survives but cost_used values re-base (fewer, fatter ops) - a
product decision to version, same as any replay-affecting change. Subsumes
several /OPTIMIZATIONS.md entries (field-update fusion, GET/SET cache-slot
sharing become natural in a register IR).

### 2. Flatten the interpreter into an explicit State-owned frame stack (B-O1; structurally fixes #2)

Today every Lua-to-Lua call recurses through Rust (`State::call` ->
`eval_closure` -> `eval_closure_inner` -> `Frame::eval`), with per-call
costs that are all consequences of the frame being a Rust stack local:

- `Closure` clone per call (`GcHeap::as_lua_function`,
  `object.rs:237-241`): clones the upvalues `Vec<UpvalueRef>` (heap alloc
  per call for any closure with upvalues) + 2 Arc refcount bumps.
- Duplicate frame bookkeeping: `CallInfo` push (another `Arc<Bytecode>`
  clone, `eval.rs:304-307`) parallel to the `Frame` itself.
- `stack.remove(idx)` to extract the callee (O(args) memmove, `eval.rs:68`).
- varargs `drain(..).collect()` Vec per vararg call (`eval.rs:310-314`).
- return values `drain(..).collect()` into a fresh Vec then `extend` back
  (`eval.rs:404-416`) - an allocation per returning call.
- per-call string-literal interning (see #8 below).

A single dispatch loop over a State-owned `Vec<FrameState>` (bytecode Arc,
ip, base, vararg span, cache ptr) eliminates every item above, merges
`Frame` with `CallInfo` (stack traces read the real frames), and -
critically - makes frames visible to `mark_gc_roots`, which structurally
fixes bugs.md #2 (unrooted varargs) instead of patching it. Calls become
"push frame record, continue loop"; returns become "memmove rets down, pop
frame record". MAX_CALL_DEPTH becomes a plain length check, and the
AGENTS.md concern about Rust-stack bloat per recursion level disappears.
Highest-leverage rewrite in the execution core; call-heavy benches
(`calls/*`, `benchmark`) are the ones sitting at ~4x lua5.5. Pre-1.0,
internal-only: `State::call`'s public signature can stay.

### 3. 8-byte NaN-boxed `Val` (C-O7)

`Val` is currently 16 bytes (tag + f64). NaN-boxing (payloads in the NaN
space of an f64: slotmap keys are 64-bit but their useful entropy fits
48-51 bits with an index/generation split; RustFn would need a registry
index rather than a raw pointer) halves stack/table/upvalue memory traffic
and makes `stack: Vec<Val>` copies twice as dense. Full-rewrite class:
touches every `match` on `Val`, but it is mechanical, determinism-neutral,
and is the standard reason reference VMs beat tagged-enum interpreters on
memory-bound workloads (`tables/fill`, `fields/*` are exactly the
3-4x-behind benches). If NaN-boxing is judged too invasive, a cheaper
intermediate is boxing only `Table` storage entries more densely; but the
full version is where the payoff is.

Note the RustFn registry index is now a cost of this item rather than a
shared prerequisite: `Val::RustFn` compares and hashes by function address
and renders as a constant `<function>`, so nothing outside NaN-boxing needs
an id. Squeezing a raw `fn` pointer into the NaN payload is the part that
would force one.

### 5. Iterative graph walks: GC mark + save walker + bytecode rebuild (C-O6 + D-OPT-1; removes the #11/#27/#28 abort class)

One rewrite kills three unbounded recursions: replace `GcHeap::mark_children`,
`SaveBuilder::encode_object`, and `build_bytecode` with explicit worklists
(Vec-based stack of pending ObjectPtrs / chunk ids, iterate until empty).
Determinism unaffected (traversal order can be made identical to today's
recursion by pushing children in reverse). Marking via worklist also tends
to be faster than deep call recursion for big heaps (no per-object call
frame, better locality), so this is a perf win in `gc_collect` - one of the
two `#[hotpath::measure]` internal hot paths - not just a robustness fix.
It also opens the door to reusing one scratch `Vec<ObjectPtr>` across
collections (zero-allocation marking) and removes the hotpath-annotation
recursion caveat for `mark`. This is the "do not preserve structure just
because it exists" case: the recursion is only there because it was easy.

---

## Structural / moderate changes

### 6. Token-stamped line numbers (A-O2; kills quadratic parse behavior; infrastructure for the #9 fix)

`update_line` (parser.rs:392-395) calls `line_and_col`, a linear walk of the
whole `linebreaks` vec (lexer.rs:489-505), at every statement start and
every `expect()`. Parse time is therefore O(statements x lines) - quadratic
for large scripts; a 10k-line chunk does tens of millions of window compares
inside `parse_str`, a measured hotpath. Fix: the lexer already knows the
current line when it produces a token - stamp `Token` with `line: u32` (fits
existing padding) and make `update_line` a field copy; keep `line_and_col`
(binary-searchable via `partition_point`, the vec is sorted) only for error
rendering.

### 7. Stop using `Vec::remove` in call emission (A-O4; root-cause fix for #9)

Every plain call does `code.remove(mark_idx)` (expr.rs:272, 327): O(n) tail
shift per call, so call-heavy chunks are quadratic-ish in emission. `remove_instr`
keeps `line_info` aligned, so it is not a line-attribution bug. Options: (a) introduce OP_NOP and
patch the mark in place (O(1), keeps line_info aligned; one extra cheap
dispatch on calls that needed the mark removed - or strip nops in `finalize`
with a jump-offset remap done once, correctly, in one place); (b)
restructure so the mark is only emitted once the arg list is known to need
it (requires buffered args or a pre-scan). (a) is the pragmatic fix.

### 8. Intern chunk string literals once per (State, Bytecode), not per call (B-O2)

`initialize_frame` (eval.rs:424-443) re-interns ALL of a chunk's string
literals into `state.string_literals` on every call, and truncates them on
return - k intern-pool probes + pushes + per-literal GC-threshold checks per
call, where k counts every field name and string constant in the function.
Pure per-call overhead for steady-state code. Replace with a per-State cache
keyed on `Bytecode` identity holding the interned `Vec<Val>`, marked as a GC
root (or pin literal strings for loaded programs). Same
`IndexMap<*const Bytecode, ...>` infrastructure /OPTIMIZATIONS.md already
sketches for sharing `RuntimeCaches` across closures - one mechanism can
serve both, so fold them into a single work item. `get_string_constant`
then becomes a direct slice index and the `string_literal_start` frame
plumbing disappears.

### 9. SET_GLOBAL inline cache and allocation-free global writes (B-O3)

`instr_set_global` (eval_store.rs:361-379) does UTF-8 validation + `String`
allocation + IndexMap hash lookup on every global assignment;
`set_global_value_owned` re-checks `Builtin::from_name`. GET_GLOBAL already
has an IC (`globals_version` + index). Mirror it for writes: cache the entry
index per SET_GLOBAL site (slot in `RuntimeCaches`), validated by
`globals_version`; on hit, write through `globals.get_index_mut` with zero
allocation. The builtin check can be resolved at compile time (the parser
already emits SET_BUILTIN for known names, so the runtime `Builtin::from_name`
re-check is only needed on the cold path).

### 10. Gate the table-library fallback off the field-read miss path (B-O4 + C-O5, two independent proposals)

Every plain-table field read that misses (no metatable, key absent) - the
extremely common `if t.optional_field then` pattern - takes
`push_table_library_field` (eval_index.rs:37-39, 350-363): a global lookup
of `table` plus a full `get_table_with_key` against the library table,
before returning nil. The fallback feature (`t:insert(...)` sugar) only ever
resolves the handful of table-lib method names, and the name set is static.
Cheap gate: check the key against a compile-time set of table-lib names
(they are string literals; a per-chunk bitmask computed at literal-pool
build time, or an interned-ptr comparison against the few lib method
strings) and skip the fallback entirely for all other keys - no cache
invalidation at all. Alternatively only emit the fallback-capable GET_FIELD
variant in method-call position, where the sugar is actually used. Note the
tracked "Table-library fallback IC" entry in /OPTIMIZATIONS.md optimizes the
HIT case; this kills the much more common MISS case and is simpler.

### 13. Version-validated cursor for `pairs` (C-O4)

`instr_tfor_call_next` (eval_control.rs:186) calls `Table::next(&control)`,
a full hash lookup (`get_index_of`) of the control key on every iteration -
`pairs` over an N-entry Map table is N hash probes. The TFOR fast path
already special-cases `base_next` by fn address, so it can carry a hidden
numeric cursor: cache `(table_ptr, version, index)` in the loop's control
area (or a per-frame side slot); on each step, if ptr+version match, step
`get_index(index + 1)` directly; on mismatch, fall back to the key-based
`next`. Deterministic (same order as today), invisible to scripts, and it
composes with the #6 fix (a tombstone-aware `get_index` walk skips dead
entries without hashing). `iter/pairs` is a headline bench (86ms, 2.0x
lua5.5) - this removes the dominant per-step cost.

### 14. Stop cloning `Closure` on every Lua call (C-O3 + B-O5, two independent proposals)

`State::call` -> `Val::as_lua_function` -> `GcHeap::as_lua_function`
(object.rs:237-242) deep-clones the `Closure`, including
`upvalues: Vec<UpvalueRef>`, on every Lua-to-Lua call - a heap allocation
per call for closures with captured upvalues, avoidable copying for all
calls. Options, increasing invasiveness:

- `Closure.upvalues` as `Arc<[UpvalueRef]>` (clone becomes a refcount bump;
  `RuntimeCaches` and `Bytecode` are already Arcs, making the whole clone
  trivially cheap) - C estimates ~all the win for a 3-line diff; or a
  SmallVec inline <= 2;
- or restructure `eval_closure` to take `ObjectPtr` and borrow the closure
  from the heap per access (bigger borrow-model change).

Benches `calls/*` should move; `alloc/closure` guards against regression.
Subsumed by #2 if that lands.

### 15. gmatch: drop the Lua-side wrapper and table-backed iterator state (E-O2)

Partially shipped as of 2026-07-25 - narrow this item before working it. The
per-iteration `to_vec` copies of subject and pattern and the full pattern
re-validation (`from_bytes_try`) are gone; `string.rs` now has
`memoize_gmatch_pattern` and `gmatch_subject_matcher_and_cost_meter`. That was
the bulk of the win, and `strings/patterns` moved 160ms to 66ms on the back of
it.

What remains (string.rs:19, 519-558, 739-751): one Lua call into the compiled
wrapper chunk (`gmatch_wrapper()`, still a compiled Bytecode) + one RustFn call,
plus three `get_table` lookups ("s", "p", "pos") and one `set_table_raw` against
the table-backed iterator state, every iteration. A generic-for iterator already receives `(state, control)`, so a
RustFunc can be the iterator directly; hold the iterator state (subject Val,
compiled pattern, pos) in a Rust-side object (heap object variant or
registry anchor) instead of a Lua table of strings. Order-of-magnitude
reduction in per-iteration work for `for w in s:gmatch(...)` loops. The
wrapper/table design mainly serves the snapshot feature; keep a serializable
representation for that (the state is just (string, string, number)).

### 16. gsub: precompile the replacement template (E-O3)

`append_gsub_replacement` re-fetches and re-copies the replacement bytes
(`to_bytes_coerce(3)?.into_owned()`) and re-parses `%N` escapes for every
match (string.rs:139-214). Parse the template once per gsub call into
`Vec<Segment{Literal(range) | Capture(n)}>`; per-match work becomes pure
byte appends.

### 17. Constant folding, with reference Lua's guards (A-O6)

There is no folding at all: `local ms = 60 * 1000` multiplies at runtime on
every execution, `-5` is PUSH_NUM + NEGATE per hit, and both charge cost.
Fold literal arithmetic/unary at parse time with lcode.c's guards (refuse to
fold results that are NaN or 0.0, exactly to avoid the -0.0 literal-pool
collapse: `find_or_add_number` dedups with `==`, which conflates 0.0/-0.0).
Note: folding changes cost_used for identical source - same
replay-versioning consideration as #1.

### 18. Compare-and-branch fusion (A-O7)

`while i < n do` emits LESS (push bool) + BRANCH_FALSE (pop bool) - two
dispatches plus stack traffic per iteration of every hot loop condition. A
peephole in the parser (or in `finalize`) fusing comparison + branch into
one opcode halves the dispatch on loop headers. Needs execution-corner
cooperation for the new opcodes.

### 20. Skip CLOSE_UPVALUES in closure-free functions (A-O9)

`level_down` and every loop parser emit CLOSE_UPVALUES unconditionally at
scope exits (parser.rs:369-383; stmt.rs:259, 340, 441-443, 479-481) -
including once per iteration inside numeric/generic for bodies. If a
function body creates no closures (`chunk.nested` empty and no OP_CLOSURE
emitted), no upvalue over its locals can ever be open, and every
CLOSE_UPVALUES in it is a guaranteed no-op paying a dispatch. A `finalize`
pass can prove this per-Bytecode and drop/nop them. Most game-script hot
loops are closure-free; this removes a per-iteration dispatch from all of
them.

### 21. Sweep the upvalue pool during GC (D-OPT-5 + C-E3)

`src/vm/object.rs:51-53` justifies never freeing upvalue slots with "VMs
have short lifetimes", but the snapshot feature exists precisely for
long-lived, save/load-cycled States, and the product runs continuous
per-tick callbacks. Every closure-with-captures created over a session leaks
a pool slot forever (`UpvaluePool::alloc` only grows); ironically a
save/load round-trip compacts the pool (only reachable closed upvalues are
serialized). Sketch: sweep the pool during `gc_collect` - mark reachable
`UpvalueRef`s while marking closures (the infrastructure already threads
`upvalue_pool` through marking), then thread freed indices into a free list
consumed by `alloc`. Determinism holds because allocation order stays a pure
function of execution history.

### 22. `parse_chunk`: `mem::take` instead of two full Bytecode clones per nested function (A-O3)

parser.rs:462 (`outer_chunks.push(self.chunk.clone())`) and parser.rs:487
(`let tmp_chunk = self.chunk.clone()`) deep-copy code, literal pools,
line_info, and nested Arcs of the partially built outer chunk and the
finished inner chunk, then immediately overwrite the originals.
`std::mem::take` / `std::mem::replace` make both O(1). Cost today is
O(enclosing-chunk size) per nested function definition - top-level files
with many functions pay repeatedly.

### 23. Zero-allocation identifiers and names in the front end (A-O5)

- `lex_word` (lexer.rs:472-485) builds a fresh `String` for every
  identifier/keyword token, used only for `keyword_match`, then thrown away
  (the parser re-slices the source anyway). Match on the
  `&source[tok_start..pos]` slice instead: one alloc per token removed on
  the front-end hot path.
- `locals: Vec<(String, i32)>`, `upvalues: Vec<(String, ...)>`, and
  namelists allocate a String per declaration (parser.rs:89, upvalue.rs:103,
  stmt.rs:294). The parser already carries `'a`; these can be `&'a str`
  borrows of the source.

### 24. Plain find/gsub: replace the naive subslice scan (E-O4)

`find_subslice` (string.rs:37-45) is `windows().position` - O(n*m) with no
skip loop. A memchr-based first-byte skip (or full two-way/memmem) is
deterministic and typically several-fold faster on long subjects; serves the
`string.find` plain path and the plain-gsub loop.

### 25. Stop copying subject/pattern per string-library call (E-O5)

`find`/`match`/`gsub`/`gmatch` each do `to_bytes(...)?.to_vec()` of subject
and pattern. With #4's init-offset API, matching can borrow the interned
bytes directly: the subject and pattern Vals sit at stack indices 1-2 for
the whole call, so they are GC roots; only replacement paths that can run
Lua (function/table repl in gsub) genuinely need an owned copy (Lua code can
trigger GC/interning reallocation - verify StringPool storage stability
before borrowing across allocation; otherwise copy only in those two modes).

### 26. Save walker: cheap breadcrumbs and StringPtr-keyed dedupe (D-OPT-2 + D-OPT-3)

- `encode_object`/`encode_val` (save_state.rs:313-351) build
  `format!("{path}[{idx}]")` breadcrumbs for EVERY value visited, purely to
  populate `SaveError::UnregisteredFunction.reachable_from` on the rare
  failure. Success path does O(total-values x depth) string allocation and
  copying. Track a cheap breadcrumb instead (a
  `Vec<(parent_object_id, entry_index)>` scope stack, or just the current
  object id) and reconstruct the human-readable path only when building the
  error. Falls out naturally from #5's worklist (the entry carries the
  parent link).
- `encode_val` for `Val::Str` (save_state.rs:281-292) copies the string
  content (`to_vec()`), probes `BTreeMap<Vec<u8>, u32>` (full-content
  comparisons per probe), then clones the bytes again for the id map.
  Strings are interned per-State, so `StringPtr` equality is content
  equality: key the dedupe map as `BTreeMap<StringPtr, u32>` and copy the
  content exactly once, when first appending to `strings`. Ids still
  assigned in first-encounter order, so byte-identical output.

### 27. Shape note for the bugs.md #1 fix (D-OPT-4)

Whatever shape the `with_restricted_env` fix takes, prefer the rooted-field
variant over cloning: pushing the saved env into a marked `State` field is
O(1) and keeps `mark_gc_roots` as the single source of truth, versus
deep-copying the environment (O(globals)) or suppressing GC (unbounded heap
growth during `f`). Recorded so the fix does not get implemented as "clone
everything".

---

## Micro / take-or-leave

- **Call-path cleanups worthwhile even if #2 lands later (B-O5):** replace
  return-value drain-to-Vec + extend with
  `self.stack.drain(self.stack_bottom..ret_start)` (one memmove, after
  `close_upvalues`) - removes one heap allocation per returning call.
  `concat_helper`: numbers are formatted with `format!` (a String allocation
  per number operand); write into the existing `buffer` via
  `std::fmt::Write`/itoa-style instead; also the two-pass loop reads the
  heap twice per string - single pass with a reserve is simpler and faster.
  `State::call` fixed-arg path: avoid `stack.remove(idx)` by treating the
  callee slot as frame slot -1 (adjust `stack_bottom`); pairs naturally
  with #2.
- **Dispatch micro-items, verify with asm/bench first (B-O6):** opcode space
  is sparse (0-25, 30-54, 60-63, 70-72); a dense renumbering (or
  `#[repr(u8)]` enum with a validated dense range) helps LLVM emit a single
  dense jump table without range holes. `get_instr` bounds-checks every
  fetch; with one-time validation that all jump targets are in-bounds at
  load/finalize time, the fetch could use a pointer/len cursor - only if
  profiles show it; keep panics over UB.
- **Table micro (C-O8):** `Table::get_with_index` on Map does `get_index_of`
  + `get_index` (two probes); IndexMap's `get_full` does it in one.
  `promote_to_map` allocates capacity `INLINE_CAPACITY + 1 = 5`,
  guaranteeing a rehash almost immediately for growing tables; promoting
  straight to 8 avoids one rehash on the common grow-past-inline path.
  (`try_insert_table_direct`'s double probe is only on the
  metatable-present path; the no-metatable path is single-probe already -
  fine as is.)
- **Snapshot encoder buffer pre-sizing (D-OPT-6):** `Encoder::new` starts
  from an empty Vec; a large save reallocates the output buffer log-many
  times. One-line `Vec::with_capacity` seeded from a cheap estimate
  (`strings total bytes + 16 x value count`). Only worth bundling with other
  snapshot work.
- **Stdlib micro (E):** `string.format` should format into one output buffer
  instead of per-directive Vec round-trips. `table.pack`:
  `for _ in 0..num_args { state.remove(1) }` is O(n^2) stack shuffling; a
  single rotate/truncate does it. `is_plain_lua_pattern` treats `-` as magic
  even though a `-` with no preceding class item at pattern start is
  literal; conservative is fine, just noting the fast path misses hyphenated
  plain needles like `"foo-bar"`.
