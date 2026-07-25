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

**Every finding from this hunt is now fixed**, including the deferred
saved-bytecode stack-discipline verifier that was the last one standing. What
remains below is the coverage record: the areas each corner examined without
finding anything, the claims measurement later disproved, and the orchestrator
notes. Keep it - it is the map of what has already been looked at, and it is
what stops the next hunt re-deriving the same non-findings.

---

## High severity

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
