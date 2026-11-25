# NANBOX design: 8-byte NaN-boxed `Val`

Settled rulings and link plan for the chain that replaces the 16-byte
tagged-enum `Val` (`src/vm/lua_val.rs`) with an eight-byte NaN-boxed word.
Sparred with codex (session 01a2128a-1012-7eb0-93fb-a8e13d356f22) to
consensus on every ruling below.

## Rulings

1. **Shape: `struct Val(u64)`, safe bit packing, no `unsafe`.** Numbers are
   raw f64 bits. Every non-number lives in sign-set NaN space strictly above
   `0xFFF8_0000_0000_0000`: the top 16 bits `0xFFF9..=0xFFFF` are tags, the
   low 48 bits payload. Nil, Bool, Str, RustFn and Obj each take a tag; two
   tags stay spare (distinct table/closure tags for typed arenas are a later
   item, not this chain).
2. **Handles fit in 48 bits: slot index u32 plus a 16-bit generation.**
   Landed in link 1 (arena versions are `u16`, `CACHE_PRUNE_INTERVAL` is
   `1 << 14`, const-asserted under `1 << 15`). Rejected: strong values with
   index only and full-generation weak handles. Every inline-cache hit
   compares the cached handle with the receiver's, which would need the
   slot's generation (a heap load on the hit path) or a per-sweep epoch on
   every identity cache, and strong values would lose the stale-handle check.
3. **Every number is published through one constructor `Val::num(f64)` that
   maps any NaN to `CANONICAL_NAN_BITS` with an integer bit test**
   (`numeral::canonical_nan` logic), never an f64 compare: `push_number` must
   not run FP instructions on its argument (docs/determinism.md, FP
   environment). Do not reason that arithmetic yields only the two standard
   quiet NaNs; negating a payload NaN lands in tag space. The packed word
   stays private to `lua_val.rs`; no `from_bits` constructor exists outside
   it. Typed temporaries and literal pools may stay `f64`; their publication
   into registers, tables and upvalues goes through `Val::num`. Snapshot
   decode keeps its own literal-pool canonicalization too.
4. **Equality.** Numeric equality stays f64 equality (NaN unequal to itself,
   `+0 == -0`). Word equality is valid only for non-number encodings and for
   internal bit-identity witnesses (the builtin mirror). A whole-word
   shortcut in `PartialEq` is wrong both ways.
5. **No niche.** Tables store `(Val, Val)` with vacant `(Nil, Nil)`, not
   `Option<Val>`, so `Option<Val>` stays 16 bytes as a register-pair return.
6. **GC pacing does not follow the representation.** Resident bytes charge
   fixed literals (`RESIDENT_BYTES_PER_ARRAY_SLOT` 16,
   `RESIDENT_BYTES_PER_HASH_ENTRY` 32 in `limits/tuning.rs`; landed in link
   1, equal to today's 64-bit `size_of`, so timing is unchanged on 64-bit
   targets). With an 8-byte `Val` the charge exceeds the allocation, so link
   2 widens resident accounting to `u64` end to end (capacity
   multiplication, the watermark, recomputation, the debug census, string
   payloads, suspended environments) and divides in `u64` before converting
   to the score, keeping the existing `usize` score/threshold saturation and
   the saturated-versus-disabled distinction.
7. **Observable surface: none beyond the NaN wording.** No `FORMAT_VERSION`
   bump (the wire carries no handle bits or generations), no
   `COST_MODEL_VERSION` bump. docs/determinism.md's sentence that inside the
   VM a NaN keeps its sign and payload must be rewritten in link 2 (it will
   be canonical inside too). Sorting uses numeric `<`; `%a` handles
   nonfinite values before the bit formatter.

## Link plan

- **Link 1 (landed with this document):** arena generation narrowed to 16
  bits, prune interval `1 << 14`, fixed-literal resident charges, tests and
  contract text updated (`docs/limits.md`).
- **Link 2 (one implementer, large):** `Val(u64)` in `lua_val.rs` with a
  `kind()` decode to a Copy view enum carrying today's variants, constructors
  (`Val::num`, `Val::str`, ...), and direct accessors (`as_num`, `is_nil`,
  `truthy`) for hot handlers; migrate every construct and match site (about
  1100 across `src/vm`, heaviest `table.rs`, `save_state.rs` and its
  verifier, `global_env.rs`, `numeric_loop.rs`, `metamethod.rs`,
  `table_ops.rs`; `lua_std/basic.rs`). Ruling 6's `u64` widening. A
  `const _: () = assert!(size_of::<Val>() == 8)`. docs/determinism.md NaN
  text. Re-measure `brokkr test native_bytes_per_level` dev and release and
  update `reference/native-calls.md` if it moves; save goldens must be
  byte-identical (`tests/integration/snapshot/save_golden.rs` is the
  oracle). Interleaved matched `--commit` A/B on same_obj_read, arithmetic,
  fill, list_literals and gc_churn, recorded with hash and host.
- **Link 3 (green agent):** full unscoped `brokkr check`, the golden and
  corpus-cost diffs, and the CHANGELOG entry.
