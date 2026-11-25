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
   otherwise (both widened by the fixed-offset slack of ruling 17).
   Indexing by the narrowed operand is then provably in bounds; one
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
   is borrowed: a State that never runs a wide frame pays about 2 KiB, not
   512 KiB.
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

## Rulings from link 2a (spar session 01a21343)

14. Executable operands are the canonical words. No side stream, no
    repacked stream: each register already sits in its own u16 field, so a
    narrow read is the low byte of that field, one fetch, and snapshot
    bytes and every canonical reader stay unchanged. Refused: a typed side
    stream (second fetch, more memory per instruction) and a repacked word
    (a second layout plus retained or recovered canonical words).
15. `Bytecode::code` is `instr::Code`: a private boxed slice of canonical
    words plus the tier. It derefs to `[Instr]` for every reader; any
    mutable access drops the tier to Wide first; Wide is the default and
    what a built vector becomes. Wide indexes soundly whatever the words
    hold, so only Narrow is a claim. `Code::install_tier` takes the
    verifier's verdict and also requires every word to pass
    `NarrowInstr::new`; a disagreement is a VM bug, debug-asserted, and
    leaves the function Wide in release (slower, never aliased), so no new
    error variant. The verdict is installed by the producer that computed
    it, on the words it verified, with nothing in between: compile
    finalization, and snapshot materialization from the admitted
    `SavedBytecode` words. The width check never substitutes for the
    verdict: `FOR_LOOP 253` fits a u8 while touching slot 256.
16. Narrowing is permission per field use, not per word. The opcode rows
    in `instr.rs` carry a mandatory register-use column (A, B, C marks; a
    const assertion keeps marks inside the layout). `NarrowInstr` keeps the
    full-width canonical getters and adds `reg_a`/`reg_b`/`reg_c`, valid
    only on marked fields (debug-asserted). Endpoints are unmarked:
    `CLOSE 256` and `RETURN 256, 0` are admitted Narrow, and a truncating
    getter on them would alias register 0. `RETURN` A is unmarked
    altogether (its first is only a range start, kept on the checked API);
    `CALL` A, `VARARG` A, `LOAD_NIL` A and `CONCAT` B are marked because
    the verifier always checks at least one slot there. The view
    `Code::narrow_view` -> `NarrowCode::fetch` wraps words without a
    recheck; it lives in `instr/code.rs` so the private-field invariant
    and the `NarrowInstr` constructor share one module.
17. Fixed implicit offsets are covered by window slack, amending ruling 4:
    the windows are `[Val; 256 + K]` and `[Val; 65536 + K]` with K the
    largest fixed offset a window arm adds (3: numeric for `a + 3`, the
    fused `TFOR_LOOP` back edge's `a + 3` from the step's own A, `SELF`
    `a + 1`). The offset is added to the frame-relative widened index, so
    the physical bound is provable and nothing wraps; the logical bound is
    still the verifier's (`registers(a, 4)`, `registers(a, 2)`). Ranges
    such as `TFOR_CALL`'s results at `base + 3 + i` stay checked.
18. Numeric-loop register maps stay u16, built per State with their cost
    binding; no recognition/cost split and no timing exception. Every map
    register derives from the owning frame's verified operands or
    effective ranges, which is all ruling 5 needed from them. Their stack
    accesses stay on the checked API, extending ruling 6: guard loads,
    publication, call-clear ranges and leaf parameter reads are per entry,
    per checkpoint, ranges, or address another function's frame (a leaf
    call writes `function + 1 + register` in the callee's frame). Not all
    are rare: `OpKind::Record` and `RecordTest` stores, `Bind` reads and
    the successful `ipairs` step's two or three writes recur per pass.
    Those are the named candidates for a later window adoption, which
    needs its own typed-access ruling and matched A/B.

## Links

1. Done (with 1a): private stack type with logical end, every crate site
   moved to it, padding machinery, no dispatch change; native-calls table
   re-measured. Matched A/B against 02589f4 measured a regression (below).
2. Done (with 2a): the shared verifier selects the tier for both
   producers; `instr::Code` carries it with the canonical words, installed
   by each producer under the `NarrowInstr` width check (rulings 14-16);
   the register-use column is in the opcode rows; tests cover the column
   at 255/256 per opcode, tier and install at the implicit-offset
   boundaries, both endpoint forms and a valued `RETURN 256`. The
   numeric-loop maps needed nothing (ruling 18). `Code::tier`,
   `narrow_view`, `NarrowCode::fetch` and the `reg_*` getters are unused
   outside tests until link 3 (`expect(dead_code)` on each, remove as
   adopted).
3. Dispatch arms (`run_frame`, `registers.rs` fast paths, `eval_control.rs`
   numeric-for) on the per-arm window. Match `code.narrow_view()` once per
   `run_frame` entry and run one dispatch body specialized per tier (the
   next entry after a call, tail call or return selects the running
   frame's tier). Compute `ra` per arm through `reg_a`, never in a shared
   prologue: unmarked fields (`CLOSE`, `RETURN`, counts, literals, jumps)
   keep the canonical getters. Window widths carry the slack of ruling 17.
   `numeric_loop.rs` stays on the checked API (ruling 18). Gate: native
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
