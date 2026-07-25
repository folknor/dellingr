# Bug hunt 2026-07-24 - consolidated findings

Consolidated and deduplicated from five independent read-only corner audits:
A (front end), B (execution core), C (data plane), D (state/persistence/host),
E (stdlib/patterns). Nothing here has been verified or executed - all evidence
is from code reading and every repro is written down but unrun. Severity is
the auditors' own ranking; where two corners disagreed the higher ranking won
and both are noted. Original corner finding IDs are kept in parentheses.
Findings reported independently by two corners are marked as such - treat
that as a confidence signal, not a verification.

Structural fixes for several of these live in [optimizations.md](optimizations.md);
cross-references below use its item numbers.

Numbering is stable: fixed items are deleted outright and their numbers are
never reused, so gaps in the sequence are expected and cross-references stay
valid. Fix history lives in git.

---

## High severity

### 16. Cost-model gap: pattern matching and string byte-work are entirely uncharged (E-E13; table.concat also C-D2)

- **Locations:** no `consume_cost` anywhere in `src/lua_std/string.rs`,
  `src/lua_std/string_format.rs`, or `src/patterns/`;
  `src/lua_std/table.rs:194-250` (`table.concat` charges 1 for O(j - i) work
  and arbitrarily large output).
- **Cause / consequences:**
  - A script can build a ~131k-char string with ~17 concat ops (repeated
    doubling), then each `gsub`/`find`/`match`/`upper`/`format("%s")` call
    does O(n) or worse work for ~0 charged cost, repeatable in a loop.
  - Backtracking patterns are superpolynomial in time while the depth cap
    only bounds recursion: `"a*a*a*a*b"` against a long non-matching subject
    of `a`s does O(n^k) `singlematch` work inside `max_expand` for a single
    costed-at-0 call.
  - `table.concat(t)` over a large array does `len` lookups and builds an
    arbitrarily large byte vector for cost 1.
  (`table.insert`/`remove` charging 1 for O(n) is already acknowledged in
  OPTIMIZATIONS.md; the string/pattern side is not tracked anywhere.)
- **Measured:** a script doing ~200k `table.move` operations plus 200 `gsub`
  passes over a 65536-byte string reports `Cost used: 2` under `--limit 100000`.
  The `table.move` half is fixed; the string half still costs zero.
- **Status: fully specified, deliberately not implemented.** This is a
  **breaking cost-model change**, not a bug fix - adding charges can only
  increase measured cost, and there is no conversion formula because the
  increase depends on string lengths, output expansion, match success,
  captures, and backtracking shape. Every embedder must re-measure their tick
  budgets before adopting it. That is a release decision, so it is staged
  separately rather than folded into the `table.move` fix.
- **Infrastructure already landed:** the neutral `CostMeter` exists outside
  `vm` and is usable by the matcher without exposing `State`, and
  `cost_budget_configured` is persisted, so this is ready to implement.
- **Fix plan (agreed, from the loop-9 review):**
  - Unit is one per deterministic logical work item: one table element visited,
    one byte processed or emitted, one matcher primitive. No hardware-inspired
    divisor - existing costs are already semantic (table construction is one
    per element, `table.sort` is `n`, not `n log n`). Per operation,
    `cost = max(1, sum of work units)`.
  - Matcher charge points, threaded via `CostMeter`: every `patt_match`
    entry/continuation, every `singlematch`, every pattern byte inspected
    locating or testing a bracket class, every subject byte inspected by `%b`,
    every byte compared by a backreference, every literal-search comparison.
    Pattern validation reserves `pattern.len()` up front. Do **not** batch as
    one-per-K: that makes the stopping boundary depend on K and lets a
    pathological matcher overshoot by K.
  - Per function: `len` free; `sub` returned bytes; `upper`/`lower`/`reverse`
    input length; `find`/`match` pattern length + steps + materialized result
    bytes; `gmatch` validation at creation then steps + captures per iteration;
    `gsub` pattern + search work + replacement bytes examined + emitted bytes;
    `format` format bytes scanned + output bytes; `table.concat` elements
    visited + output bytes including separators. Both sides for constructors -
    a no-match `gsub` charges once to scan and once to copy.
  - Output buffers charge before mutating, since `gsub`/`format`/`concat`
    output size is unknown at entry. Matcher work must be committed before a
    `gsub` replacement function re-enters Lua.
  - Needs `MatchError::{Pattern, Budget}` so exhaustion surfaces as
    `ErrorKind::BudgetExceeded` rather than being flattened into the existing
    pattern `RuntimeError`.
  - Export a `COST_MODEL_VERSION`; release as the next pre-1.0 breaking minor.
    Do **not** feature-gate a legacy mode - the old behaviour is the hole, and
    compile-time modes fragment contractual costs. Validate the cost-model
    version in snapshots so old-unit counters cannot silently continue.
  - `analyze_cost` stays numerically unchanged (it cannot resolve data lengths,
    or even prove a call reaches the stdlib, since globals are mutable). Its
    docs and the CLI output should say so explicitly. Pin with a test:
    `analyze_cost("return string.upper('abcd')") == 0` while execution costs 4.
- **Related:** `OP_CONCAT` being free is separate (#24) and should not be
  folded in - though while it stands, a large subject remains cheap to build.

---

## Medium severity

---

## Low severity

---

## Coverage with no finding (merged from all five corners)

**A (front end):** determinism - the whole front end is Vec-scan based (no
HashMap, no entropy, no platform-dependent behavior); decimal literals via
Rust's correctly-rounded `str::parse::<f64>`, hex via in-crate `numeral.rs`;
identical source -> identical Bytecode byte for byte. `numeral.rs`
round-to-nearest-even with sticky bits, subnormal/overflow paths, exponent
saturation all consistent with IEEE semantics and the C30/L16 tests.
`Instr::call(ArgCount::Fixed(255))` in re-emission paths round-trips
correctly (255 encodes back to Dynamic) - fragile-but-correct; a `from_u8`
round-trip would read better. `code.remove(mark_idx)` index safety (jump
offsets, break-jump indices, table-template indices, tail-call indices) all
verified safe. Repeat-until scoping (C29),
close-upvalue emission at scope/break boundaries, multi-assign ordering,
`...` restriction, method-call desugaring, escape decoding bounds: correct.
`assign_cache_slots` bounds and deterministic slot assignment: correct.

**B (execution core):** budget boundary - `add_cost!` batching
(`frame.rs:147-159`) plus `consume_cost` (`vm.rs:329-339`) enforces exactly
"the op that crosses the boundary completes; the next costed op fails",
including flushes before OP_CALL/OP_TFOR_CALL/OP_RETURN and metamethod
invocations. One soft edge: up to 63 accumulated cost is dropped (never
added to `cost_used`) when a frame errors mid-batch - reporting accuracy
only (also noted by C). Dynamic SET_LIST sentinel is count==0 (not 255);
`analyze_cost` adds 0 for dynamic constructors, consistent with the runtime
minimum; base validation and error-path watermark truncation of
`vararg_call_bases`/`table_constructor_bases` hold. 255 ceilings (Dynamic
args, RetCount::All, `__call` prepend, `unpack`/`select`) all error cleanly;
`__call`-chain recursion bounded. Cache-slot aliasing: `finalize` rewrites
every GET_GLOBAL/GET_FIELD/SET_FIELD with a real slot index, so plain
constructors' implicit index 0 never reaches the runtime.
`with_restricted_env` swaps are honored by all three IC families via
`globals_version`, restore-under-panic via catch_unwind. `mark_gc_roots`
otherwise sound for the execution core (active closures and scoped temporaries
via `transient_roots`, per-frame literals via `string_literals`, metamethod
key/val protection pushes present).

**C (data plane):** `StringPool.hash_index` uses IndexMap keyed by pinned
FxHash values; iteration only during sweep; insertion-order semantics keep
it honest. Anchor `state_id` from a process atomic is documented and never
script-observable. Registry/slotmap iteration orders are
insert/release-history deterministic. Recently-fixed items re-scrutinized:
interned strings counting toward the GC threshold (`allocation_count`,
`is_full`, `collect` recompute) is coherent; the `usize::MAX`
auto-GC-disabled sentinel is correctly preserved across explicit
collections, and `saturating_mul(2).max(20)` cannot produce the sentinel for
any real heap size. `table_sort` charges `n.max(1)` BEFORE the comparator
runs (correct per the L18 contract). Anchors: registry correctly in the root
set; generational keys + state_id make stale/cross-state handles error
cleanly; `anchor()` validates before popping.

**D (state/persistence/host):** saturating `consume_cost` correct
(`saturating_sub_unsigned`/`saturating_add`; covered by
`consume_cost_saturates_large_host_charges`); the eval-loop batcher flushes
eagerly so batching cannot defer the boundary. Empty-state snapshot
round-trip correct (`has_standard_environment` false path,
`State::empty_with_callbacks` on load, no env tokens). on_error for
host-direct RustFunc failures fires exactly once (`call_depth == 0 &&
stack_trace.is_empty()` guard); Lua-frame path fires in `eval_closure_inner`.
Panic-safe restricted-env restoration is correct as far as it goes (but see
#1). Documented contracts: math determinism doc matches code; snapshot
versioning matches (FORMAT_VERSION strict equality is the gate, crate
version read-and-discarded); `analyze_cost` "neither lower nor upper bound"
doc matches. `VmRng` fully deterministic, pinned by test; seed 0 default
documented; `random_range_i64` degenerate-range and bias behavior documented
and sane. Save output deterministic (BTreeMaps + traversal order +
insertion-order globals; aliased fn addresses resolve to the
lexicographically-smallest id independent of registration order). Decoder
reservation capping (`len.min(remaining)`) defangs forged length prefixes;
memory linear in input size. `validate_quiescent` covers all transient
stacks/counters; eval_closure watermarks (L8) keep the State quiescent after
killed callbacks. Non-findings: anchored-only values not serialized is
documented (`SaveDiagnostics::anchor_count`); `rust_fn_ids_by_addr`
collapsing ICF-folded functions is safe (identical code, identical
behavior); cost/rng values are host-forgeable, so hosts that load
user-editable saves must reset the cost budget after loading.

**E (stdlib/patterns):** `VmRng::random_range_i64` uses i128, no overflow
from `math.random` extremes. RustFunc returning exactly 255 results does not
collide with the `RetCount::All` sentinel. gmatch's leading-`^` handling
matches reference (treats `^` literally). gsub empty-match advancement,
anchored gsub, `$`/`^` anchors at `init`, `[]]`/`[^]]` class closing,
`%b((` validation, `()` position-capture positions with `init` bases: all
line up with reference. string_format: directive length/width caps, flag
validation per conversion, `#o`/`#x` prefixes, `%g` significant-digit
selection and zero-stripping, zero-padding after `0x` for `%a`, `%q` quoting
table (incl. 3-digit escapes before digits), `%c` mod-256,
integer-representation errors - all consistent with the 5.4 contract as far
as static reading can tell. Rust's round-half-even float formatting matches
glibc's.

---

## Deferred hardening

### 59. Saved bytecode stack discipline is not verified (phase 2)

- **Locations:** `src/vm/eval.rs:235,407,434`, `eval_index.rs:436`,
  `eval_store.rs:454`, and the bytecode dispatch paths for `DUP`, `SWAP`,
  `MARK_CALL_BASE`, table initialization, field/table stores, numeric and
  generic loop helpers, fixed `SET_LIST`, and `GET_TABLE`.
- **Cause:** phase 1 validates bytecode structure, operands, cache layout, and
  nesting, but deliberately does not perform abstract operand-stack dataflow
  or marker-stack/CFG-join verification. Forged code can therefore still
  underflow `pop_val`, `DUP`'s `.last().expect`, `SWAP`'s `len - 1`/`len - 2`,
  `CONCAT A`'s `len - A`, fixed `RETURN n` while locating return values,
  `RETURN RetCount::All`'s `stack.len() - frame_base`, direct local accesses,
  loop-local ranges, a later `get_top()` call below `stack_bottom`, or open
  upvalues after malformed code has popped frame slots.
- **Fix sketch:** add the deferred stack-discipline verifier with abstract
  stack heights, vararg-call and table-constructor marker stacks, dynamic
  result counts, and agreement at CFG joins. It needs compiler-corpus proof
  before it may reject saves.
- **Status:** this is now the *only* thing standing between phase 1 and the
  stated promise ("malformed save structure is rejected with a `LoadError`; it
  cannot trigger an indexing, stack-underflow, or recursive-traversal panic
  during load"). The recursive-GC half of that caveat is gone: marking and save
  encoding are both iterative, so a deep decoded table graph no longer
  overflows during the `gc_collect()` at the end of materialization.

## Orchestrator notes (carried from the corner reports)

- E's verification list: (1) resolved with #36; (2) `table_remove_at(1, 0)` /
  `(1, len+1)` semantics vs reference's `t[0]` read/write edge (vm-side, was
  out of E's corner); (3) resolved with #12 - the test now pins reference's
  exact 199/200 depth boundary instead of asserting the divergence.
- B ordered findings most-severe-first without labels; severities shown here
  for B items are the consolidator's placement of B's ordering, not new
  adjudication.
- **Claims that measurement disproved.** Recorded so nobody re-derives them.
  All were code-read against the wrong reference version or simply wrong; the
  audit said up front that nothing in it had been executed.
  - #57's two `tonumber` claims (deleted in an earlier session).
  - `math.random(m, n)` with `m > n`: dellingr reports argument `#1`, which is
    what **5.4** reports. Only 5.2 says `#2`. "Fixing" this would introduce a
    divergence.
  - `string.format("%u")`: not removed in 5.4. Both 5.2 and 5.4 accept it.
  - `string.format("%p")` printing `(null)`: correct. 5.4 substitutes `(null)`
    for a null pointer, and 5.2 rejects `%p` entirely.
- **Deliberate divergences are documented in README**, not tracked here:
  zero-step numeric `for` (skips, where 5.2 loops forever descending and 5.4
  errors), no implicit string-to-number coercion in arithmetic or numeric
  control expressions, and `math.log` base 2 following 5.2 rather than 5.4.
