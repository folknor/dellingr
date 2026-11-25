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

## Links

1. Private stack type with logical end, every crate site moved to it, padding
   machinery, no dispatch change. Gate: full `brokkr check`, native bytes
   unchanged in release, a matched A/B showing no regression. Large: every
   `state.stack` site in `src/vm` and `src/lua_std`.
2. Executable operand narrowing and tier selection at both producers, with
   verifier-side tests for implicit offsets and forged snapshot operands at
   tier boundaries. Medium.
3. Dispatch arms (`run_frame`, `registers.rs` fast paths, `eval_control.rs`
   numeric-for, `numeric_loop.rs` entry) on the per-arm window. Gate: native
   bytes dev and release, matched A/B on closed_mode, benchmark, wide_sweep,
   key_sweep, arithmetic. Medium-large.
4. Green agent: full gate, `reference/` and `docs/` touched by the stack
   contract (Frame doc, `docs/limits.md` value stack, native-calls table),
   close the notes entry with the measured result, delete this file.
