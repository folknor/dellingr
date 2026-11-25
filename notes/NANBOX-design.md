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

### Link 2 implementation decisions

- In tag order, the encodings are nil, boolean, string, Rust function and
  object. Nil carries zero and booleans carry zero or one. Arena payloads
  put the slot index in the low 32 bits and the nonzero generation in the
  next 16, matching the existing arena-key hash input. String and object
  codecs reconstruct typed handles; the packed word stays private.
- Numeric dispatch and array indexing use the direct numeric accessor.
  The cached string slot check compares words after checking the string
  tag, preserving handle identity and avoiding redundant unpacking on the
  cache hit. The builtin mirror uses the same word identity check so it
  distinguishes the two zero signs and accepts identical canonical NaNs.
- Host numeric reads use the invariant established on publication. Literal
  pools and the snapshot wire codec retain their own handling because
  they also carry numbers outside the runtime value representation.
- The resident score divides before applying the existing saturating
  integer conversion. Watermarks use saturating multiplication in u64;
  the threshold mode and score remain independent of the watermark sentinel.

## Link plan

- **Link 1 (landed with this document):** arena generation narrowed to 16
  bits, prune interval `1 << 14`, fixed-literal resident charges, tests and
  contract text updated (`docs/limits.md`).
- **Link 2 (implementation complete):** packed `Val(u64)`, migrated VM and
  library uses, widened resident bytes, and updated the NaN contract.
  The tag-collision regression test covers signaling NaNs, negative payloads
  in tag space, IEEE equality, and bit identity for signed zero. The native
  stack reference records the dev and release probes on bygg against base
  63d34b0. Compare 63d34b0 and the link 2 commit with matched interleaved wall
  benches for same_obj_read, arithmetic, fill, list_literals and gc_churn.
- **Link 3 (green agent):** full unscoped `brokkr check`, the golden and
  corpus-cost diffs, and the CHANGELOG entry.
