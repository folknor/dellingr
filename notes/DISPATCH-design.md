# Padded value stack: safe register access without per-access bounds checks

Chain design for the "Register access bounds checks" entry in
`notes/optimizations.md`. Settled with codex (spar session 01a21310) on
2026-11-25.

## Evidence

bygg, Zen 5, 4495fca, matched interleaved `--commit` pairs, 3 rounds: every
value-stack index unchecked took closed_mode -13%, composite -4%, wide_sweep
-6%, key_sweep -5% (3 of 3 each), arithmetic flat. Release asm at HEAD: each
register access reloads the `Vec` length from `State` and pays `cmp`/`jae`;
an arm with three accesses pays three. The loop runs at IPC near 6, so the
checks cost issue slots.

## Rulings

1. No `unsafe` register or fetch access. The verifier is a correctness proof,
   not a memory-safety boundary for snapshot input.
2. `State.stack` becomes a private type: an initialized backing buffer plus a
   logical end. Every existing stack operation (len, push, truncate, resize,
   extend, drain, insert, remove, slices, the GC root walk, snapshot
   quiescence, the stack cap and host stack API) works on `buf[..end]` only.
   Moves shift the logical suffix only, never the padding.
3. Padding beyond the logical end stays initialized and is never rooted or
   visible. Stale values may remain there on shrink (no clearing); growth
   from the logical end writes or nil-fills every newly live slot before a
   collector, callback or reader can see it.
4. Dispatch gets one special view: a fixed-size array window starting at the
   frame base, in static tiers: `&mut [Val; 256]` for functions whose highest
   effective register fits in `u8`, `&mut [Val; 65536]` with `u16` operands
   otherwise. Indexing by the narrowed operand is then provably in bounds; one
   range check per borrow remains. No masking: an invalid index must not alias
   a valid register.
5. Tier and narrowed operands are fixed when executable code is built, by
   checked conversion after verification, for both producers (compile and
   snapshot load). The tier covers effective registers including implicit
   offsets (`SELF`'s `a + 1`, numeric-for `a + 3`, iterator result ranges,
   fused-handler operands, numeric-loop mirror maps).
6. The padded window proves only physical bounds. It is used only by ordinary
   arms that run while `frame.top <= logical end`. Open-result consumers,
   call windows, result ranges, one-past endpoints, stack shifts, open-upvalue
   dereferences (which may address an enclosing frame), numeric leaves and
   unwinding stay on the logically checked API. A debug assertion of the
   whole-frame invariant at the window borrow is allowed only on those
   ordinary paths.
7. Borrow per arm first: copy inputs, release before any `&mut State` helper,
   reborrow by index after it returns (the buffer may have reallocated; values
   a capture may have changed are re-read). Executor-scope amortization (one
   borrow across several arms, which needs the executor and boundary-driver
   split) is a later link adopted only on its own matched A/B.
8. Padding is allocated to `max(logical end, base + tier width)` when a window
   is borrowed: a State that never runs a wide frame pays 2 KiB, not 512 KiB.
   Retaining it as a high-water mark is acceptable.
9. Instruction fetch stays `code[*ip]` checked. A paged executable stream would
   make the fetch index a `u8` but adds page-transition dispatch and live
   cursor state for at most the 1-3% an unchecked fetch measured; refused.
10. Link 1 gives ordinary checked access through the live slice, including
    GC and host views. Stack mutations live on the private type, with eager
    range removal because every former drain discarded its removed values.
    The window helper takes a const width so both future tiers use the same
    padding code. Dispatch will supply those widths in link 3.

## Ruling from link 2

13. The shared verifier selects the register tier from the highest slot in
    its checked register ranges. Frame reservation alone is insufficient:
    a saved function can reserve slots that no instruction addresses.
    CLOSE and empty fixed RETURN are endpoints and do not raise the tier.
    Compile finalization and snapshot materialization attach the same tier
    to Bytecode. Snapshot serialization still uses the original instruction
    words and fields.

## Open decision from link 2

Settle the executable operand representation and numeric-loop plan ownership
before finishing narrowing. The canonical instruction word mixes register
indices with counts, pool indices and jump offsets. A typed operand side
stream can preserve those words for dispatch and serialization but adds a
second fetch and more memory per instruction. A repacked executable word
can use one fetch but needs a layout and a way to retain or recover the
canonical words, including wide non-register operands.

NumericLoops currently builds fused regions and frame-register maps per
State in BytecodeRuntime. Recognition takes the State's CostState and the
result carries the selected weights. Ruling 5 places narrowing of those
maps at compile and snapshot load, where no State cost model is available.
One route is to split cost-independent recognition and register mapping
from per-State charge binding. Another is to permit checked narrowing of
the derived maps at runtime-bundle construction while keeping the tier and
canonical instruction operands fixed at the producers. The latter needs an
explicit ruling on the timing exception. Decide both the representation
and the map ownership in spar before dispatch is built against them.

## Links

1. Done (with 1a): private stack type with logical end, every crate site
   moved to it, padding machinery, no dispatch change; native-calls table
   re-measured. Matched A/B against 02589f4 measured a regression (below).
2. Partial: shared verifier selects the tier for both producers, with
   tests for implicit offsets, one-past endpoints and forged snapshot
   operands at tier boundaries. Executable operands remain outstanding.
   2a. Resolve the open decision in spar, then build checked executable
       operand narrowing for both producers and cover the derived maps as
       ruled. Medium, or medium-large if numeric-loop recognition and cost
       binding need to be split. Finish before link 3.
3. Dispatch arms (`run_frame`, `registers.rs` fast paths, `eval_control.rs`
   numeric-for, `numeric_loop.rs` entry) on the per-arm window. Gate: native
   bytes dev and release re-measured under the envelope (ruling 11), matched
   A/B against 02589f4 (ruling 12) on closed_mode, benchmark, wide_sweep,
   key_sweep, arithmetic, and release asm inspection of the live-prefix
   checks that survive outside window arms. Medium-large.
   3a. Only if 3 does not beat 02589f4: recover the non-window paths by
       reusing one live slice across several accesses within an arm, or by
       an unchecked live prefix (sparred separately, see ruling 12). Medium.
4. Green agent: full gate, `reference/` and `docs/` touched by the stack
   contract (Frame doc, `docs/limits.md` value stack, native-calls table),
   close the notes entry with the measured result, delete this file.

## Rulings from link 1a (spar session 01a21328)

11. Exact equality of native bytes per level is not a gate for any link.
    The binding contract is the envelope (`docs/limits.md`, "Envelope");
    the table in `reference/native-calls.md` is a per-build measurement and
    is re-measured and replaced when a frame moves. The "same numbers
    before and after" sentence there binds only the native-call marker
    macro. Link 1 (the commit "Move the value stack behind a private
    buffer with a logical end"; benched as its pre-amend twin 1b152ad,
    identical code), bygg, against 02589f4: release rows -64 to
    +48 (mostly +16; max 1808 to 1824), release+snapshot max 1744 to 1760,
    dev every row about +900 to +1000 (max `callable_rust` 10864 to 11856,
    about 145 KiB more at 150 levels of the largest cycle). Envelope tests
    pass. Do not assume link 3 recovers the dev growth; the probe cannot
    attribute it.
12. Link 1 stays committed on local main, but main is not pushed while the
    regression below stands. The item gate is the combined change against
    02589f4, never against link 1. A failed combined A/B keeps the push
    gate closed; a fix (slice reuse or unchecked prefix) needs its own
    fresh matched A/B of the final tip against 02589f4 plus the correctness
    and native-stack checks. An unchecked prefix (`get_unchecked(..end)` in
    the Deref) would rest on ValueStack's own `end <= buf.len()` invariant,
    not on the verifier, so ruling 1 does not cover it, but it puts a
    memory-safety obligation on every future mutator and needs its own
    ruling. If the window design is abandoned, back out every dependent
    change and verify the resulting tip, not just a revert of link 1.

Link 1 matched A/B, bygg, `--rounds 3 --commit 02589f4 --commit 1b152ad`,
invocation bb228d88, median paired difference: closed_mode +252.6 ms
(4293.5 to 4546.1, +5.9%, slower 3 of 3), wide_sweep +148.5 (+7.4%, 3 of 3),
key_sweep +79.1 (+2.7%, 3 of 3), arithmetic +26.7 (+1.5%, 3 of 3),
benchmark +43.8 (+1.4%, 2 of 3). Likely mechanism, unconfirmed in asm: each
`self.stack[i]` now checks `end <= buf.len()` forming the live slice and
then `i < end`, two checks where the Vec had one. Layout is not excluded.
