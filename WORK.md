# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #44, #45, #63: compiler capacity and line attribution

Compile-time ceilings far below reference, plus two smaller front-end defects.
All verified by execution.

| script | dellingr | Lua 5.4 |
|---|---|---|
| `{1, 1, ... }` x300 | `1:522: too many fields in table constructor (limit 255)` | runs |
| `{x1=1, ... x300=300}` | `1:2095: too many literal strings` | runs |
| `t.f1 = 1` x300 | `257:7: too many literal strings` | runs |
| `s = s + 1.5` x300 distinct numbers | `256:14: too many literal numbers` | runs |

These are correct rejections given the current encoding, not miscompiles - but
they are plausible in real init/data scripts, which is the point of the
finding.

**Note which limit actually binds.** `t.f1 = 1` x300 fails on the *string*
pool, not on `TooManyFieldAssignments`, because every field name is a distinct
string literal. Raising the SET_FIELD cache-slot ceiling alone would not fix
that case; the shared pool is the first wall in three of the four repros.

### #44 (Low) - four separate ceilings

- **255 array entries per table constructor** (`table.rs:158-160`,
  `TooManyTableFields`). Reference flushes SETLIST every 50 entries and handles
  millions. dellingr can do the same with periodic `set_list(k)` flushes and
  **no encoding change** (optimizations.md #19).
- **255 distinct string literals per function** (`parser.rs:253-271`,
  `TooManyStrings`) - one pool shared by string literals, global names *and*
  field names.
- **255 distinct number literals per function** (`TooManyNumbers`).
- **255 `t.f = v` SET_FIELD sites per function** (`compiler.rs:259-270`,
  `TooManyFieldAssignments`). The limiter is the cache-slot byte (the C
  operand), not the instruction. Sites past 255 could emit a sentinel meaning
  "not cached" and take the slow path, so capacity stops being a hard limit.
  Today every index below 255 resolves to a slot, so the sentinel must be an
  out-of-range value - keep 255 reserved as "no cache". `instr_set_field`
  already tolerates `cache = None` via `caches.set_field_lookup.get(idx)`.
- **Cosmetic:** more than 65535 GET_FIELD sites errors as `InternalError`
  rather than a `SyntaxError` (`compiler.rs:247-255`), inconsistent with the
  SET_FIELD path immediately below it. `InternalError` now means "VM bug" since
  #52 landed, so this is a taxonomy error too.

### #45 (Low) - `num_locals` over-counts with params plus sibling scopes

`add_local` (`parser.rs:85-95`) grows `num_locals` whenever
`locals.len() > num_locals`, but params are pushed without updating it
(`parser.rs:477-479`). With P params, sibling scopes re-trigger growth:

```lua
function f(a, b) do local x end do local y end end
```

ends with `num_locals = 2` though peak non-param locals is 1. It never
under-counts, so this is safe, but every call pushes that many extra nils
(`vm/eval.rs:346`) and consumes stack headroom.

Fix: `num_locals = max(num_locals, locals.len() - num_params)`.

### #63 (Low) - a multi-line call is attributed to its closing line

Found during loop 17. Reference reports the caller frame at the line where the
call *opens*; dellingr reports where it closes, because `OP_CALL` is emitted
after the argument list is parsed and takes whatever line is current then.

```lua
local value = {2, 1}
table.sort(value, function()   -- reference blames this line
  error('boom')
end)                            -- dellingr blames this one
```

Single-line calls agree, which is why every other traceback case matches.
`tests/error_handling.rs` currently pins the divergent line 4 with a comment
pointing at this finding; that expectation must flip to 2 as part of the fix.

Fix: record the line at the start of the call expression and emit `OP_CALL`
with it. Interacts with optimizations.md #7, which wants to stop using
`code.remove` in call emission - the same code path.

### Constraints

- **Bytecode is fixed-width 32-bit** (`[opcode:8][A:8][B:8][C:8]` or
  `[opcode:8][A:8][sBx:16]`). Any ceiling fix must either stay inside the
  existing operand widths or be argued explicitly as an encoding change.
- **`line_info` maps bytecode index to source line** and is used for stack
  traces and now for error position rendering. Changes to emission order must
  keep it aligned - optimizations.md #7 records that `code.remove` in call
  emission already desyncs it.
- Determinism: identical source must still produce identical bytecode.
- The snapshot format validates bytecode structurally (`verify.rs`); new
  operand meanings such as a cache sentinel must be accepted there too, or
  saved bytecode using them fails to load.
- Charge nothing new (#16).

---

## Agreed implementation plan

**Two claims above are wrong and are corrected here.**

1. **Literal pools never use `Bx`.** Every literal index is an 8-bit operand:
   `PUSH_NUM` A; `PUSH_STRING` A; `GET_GLOBAL` A; `SET_GLOBAL` A; `GET_FIELD`
   A; `SET_FIELD` B; `INIT_FIELD` B; `INIT_FIELD_PINNED` A. `Bx` on
   `GET_GLOBAL`/`GET_FIELD` is the inline-cache slot (`compiler.rs:248`, `261`),
   not a pool index. Table templates also store string indices as `u8`
   (`compiler.rs:187`). So growing a pool is a real encoding change, not a
   matter of using width that already exists.
2. **`remove_instr` already keeps `code` and `line_info` aligned**
   (`parser.rs:430`), so #63 is independent of optimizations.md #7. The desync
   warning in `notes/optimizations.md:119` is stale - correct it.

### Scope: do these

**#45** - in `add_local`, maximize `locals.len() - num_params`
(`parser.rs:91`, params pushed at `531`). Verify with a compiler unit test
asserting the nested chunk has `num_params == 2` and `num_locals == 1` for
`function f(a, b) do local x end do local y end end`; `Bytecode::num_params`
and `num_locals` are crate-visible (`compiler.rs:197`) and existing parser
tests already inspect nested bytecode (`parser/tests.rs:1346`). There is no
runtime hook for frame-local allocation, so the compiler is the right
verification point.

**#63** - the opening token is available as lookahead in
`parse_prefix_extension_inner`, before the normal-call branch
(`parser/expr.rs:246`) and the method-call branch (`311`); convert
`Token::start` via `TokenStream::line_and_column` (`lexer.rs:108`). The line
must be **carried on `PrefixExp::FunctionCall`**, because `OP_CALL` is emitted
later by `eval_prefix_exp` (`parser.rs:609`) or statement parsing
(`parser/stmt.rs:73`). Add an explicit-line emission helper. Flip
`tests/error_handling.rs:80` from caller line 4 to line 2.

**#44 array constructors** - flush every 255 pending values. Naive periodic
`set_list(k)` is **not** sufficient: `instr_set_list` starts every batch at key
1 (`eval_store.rs:437`). Use `SET_LIST`'s currently reserved `Bx` as a batch
ordinal with A remaining the count, and start insertion at `batch * 255 + 1`.
Old bytecode has `Bx == 0`, so its behaviour is unchanged. Preserve
`SET_LIST(0)` for a dynamic final call or vararg. Update the verifier's operand
rules.

**#44 SET_FIELD sentinel** - runtime is already safe: finalization assigns
slots only while `set_field_cache_len < 255` so valid slots are 0..=254
(`compiler.rs:263`), `RuntimeCaches` allocates exactly that many (`327`), and
`instr_set_field` uses `.get(cache_idx)` (`eval_store.rs:201`), taking the
uncached path on `None`. **The verifier is not safe yet**: it requires every C
operand to equal the next slot and increments a `u8` (`compiler/verify.rs:332`),
so it rejects the first sentinel after 254 - and saved-bytecode verification
delegates to the same function (`save_state/verify.rs:303`). Teach it that
C == 255 means "uncached", exclude it from the slot count, and keep requiring
non-sentinel slots to be sequential.

Cost is localized: sites 0..=254 keep their inline caches, sites 255+ do a key
lookup every execution. Instruction cost accounting is unchanged.

**#44 GET_FIELD taxonomy** - replace the `InternalError` for more than 65535
GET_FIELD sites (`compiler.rs:250`) with a line-bearing `SyntaxError`. Since
#52 landed, `InternalError` means "VM bug", so this is a taxonomy error as well
as an inconsistency with the SET_FIELD path below it.

---

## Phase 2: widen the literal pools (scope reversed by the user)

Everything above is **implemented and green**. The deferral below was argued
partly on compatibility grounds; the user has since ruled that out:

> We're not doing any compatibility or reinterpreting old instructions. We're
> pre-1.0.

So both pools get widened after all, and #44 closes completely. Old bytecode
and old saves may be rejected outright - bump `FORMAT_VERSION` and let the
existing strict equality gate reject v4 saves. **No fallback parsing, no
reinterpretation of old instruction words.**

### What the encoding allows

`Instr` is `[opcode:8][A:8][B:8][C:8]` or `[opcode:8][A:8][Bx:16]`
(`instr.rs:266`, `297`). Current literal indexing, all 8-bit:

| instruction | form | operands |
|---|---|---|
| `PUSH_NUM` | `op_a` | A = number index; **B, C free** |
| `PUSH_STRING` | `op_a` | A = string index; **B, C free** |
| `SET_GLOBAL` | `op_a` | A = string index; **B, C free** |
| `GET_GLOBAL` | `op_a` / `op_a_bx` | A = string index, Bx = cache slot (**full**) |
| `GET_FIELD` | `op_a` / `op_a_bx` | A = string index, Bx = cache slot (**full**) |
| `SET_FIELD` | `op_abc` | A = stack offset, B = string index, C = cache slot (**full**) |
| `INIT_FIELD` | `op_ab` | A = offset, B = string index; **C free** |
| `INIT_FIELD_PINNED` | `op_ab` | A = key index, B = template entry index; **C free** |

So three are trivially widenable to `Bx`, one (`INIT_FIELD`) has a spare byte,
and three are full.

### Agreed phase-2 plan

16-bit literal indices in `Bx`, 8-bit cache slots with `255 = uncached`,
instructions stay one 32-bit word. Save format goes to **v5**.

**New instruction layouts:**

| opcode | A | Bx |
|---|---|---|
| `PUSH_NUM`, `PUSH_STRING`, `SET_GLOBAL` | 0 | literal id |
| `GET_GLOBAL`, `GET_FIELD` | cache slot / 255 | string id |
| `INIT_FIELD` | stack offset | string id |
| `INIT_FIELD_PINNED` | template entry | string id |
| `SET_FIELD` | cache slot / 255 | string id (implied offset 0) |
| `SET_FIELD_AT` *(new)* | stack offset | string id (uncached) |

`SET_FIELD` needs two forms because A currently carries the receiver's stack
offset, which competes with the cache byte. That offset is nonzero **only** for
earlier destinations in a multiple assignment (`parser/stmt.rs:105-120`);
ordinary assignments and function declarations use offset zero
(`parser/func.rs:66-77`). So the common case stays cached in one word and the
rarer multi-lvalue case goes uncached. A prefix/extra-word form was rejected:
it inflates code size and complicates jump-target verification and line
attribution. A paged pool was rejected: it makes decoding control-flow
sensitive.

**Why the 255-cache cap is safe.** Slots are shared per *name* for
`GET_GLOBAL` (`compiler.rs:218-249`) and per *site* for `GET_FIELD`
(`250-260`). Measured over every tracked example with `luac5.4 -p -l` as an
upper bound: the most field-heavy file (`examples/feature_test.lua`) has 83
field-like instructions in the whole file, and the most global-heavy
(`examples/edge_cases.lua`) has 176 global reads across only ten distinct
names, all of which are dellingr builtins needing zero slots. Nothing is near
255. Measured slow-path cost on a map-backed table: 44.9 ms cached vs 81.1 ms
uncached over 2M writes - 1.81x, about 18 ns per write. Inline tables are
cheaper still (scan of at most four entries). **Instruction cost stays exactly
1 either way** (`frame.rs:399-403`).

**`INIT_FIELD_PINNED`:** widen only the key. The string id genuinely can exceed
255 - a small templated constructor late in a function can have every key above
255 - but the template entry index cannot, because templates are an optional
optimization for pure unique-named constructors (`parser/table.rs:62-67`) with
an existing fallback to presize + `INIT_FIELD` (`128-138`). Keep 255 entries
per template and 255 templates per function (`NEW_TABLE_TEMPLATE.A` stays u8),
rejecting the *optimization*, not the program, above that.

**Pools are `u16` (65536 entries), not `u32`.** That is 200x the motivating
cases, and every call already interns all of a function's string literals into
the State (`eval.rs:471-480`), so 65k distinct literals is already an extreme
workload; `u32` would need multiword encoding and a different literal-loading
architecture.

**Table templates widen in step** to `Vec<Vec<u16>>` - they hold string-pool
ids, so leaving them `u8` keeps a hidden ceiling. Signatures at
`table_ops.rs:26-34`, `object.rs:308-318`, `table.rs:90-99` all become
`&[u16]`.

**Verifier** (`compiler/verify.rs`, shared with snapshot loading via
`save_state/verify.rs:303`):

- split the blanket 255 check at `182-199`: string/number pools 65536;
  templates, nested chunks, upvalues keep their u8 limits; template entries 255
- `check_index` takes `u16`/`usize`; literal operands decode from `Bx`
- validate every `Bx` literal and every template `u16` key before indexing
- accept `A == 255` as uncached for all three cache-bearing opcodes, exclude
  sentinels from declared counts, require real slots sequential
- replace the fixed `[None; 256]` global-name array (`220-223`) with a vector
  sized to the string pool
- narrow `global_cache_slots` / `field_cache_slots` metadata from `u16` to `u8`

**Emission:** the parser emits `SET_FIELD_AT` initially; finalization rewrites
offset-zero forms to `SET_FIELD`, allocating slots 0..=254 then sentinel 255,
and applies the same policy to global and field reads. Remove the now-dead
`TooManyFieldLookups` and `TooManyFieldAssignments`. Add `SET_FIELD_AT` to
static cost analysis as one table write.

**Snapshot:** bump `FORMAT_VERSION` 4 -> 5 (`save_state.rs:56`); the strict
equality gate rejects v4 with no fallback. Templates serialize as nested `u16`
vectors rather than opaque byte strings (`1700-1718`). Decoder reservation caps
are unaffected - lengths are already `u32` and bounded by remaining input.
Update the unsupported-version test (`tests/save_state.rs:658-674`).

**Tests:** 300 distinct numeric literals; 300 distinct strings; more than 255
distinct global and field names across reads and writes; a templated
five-field constructor whose key ids exceed 255; a constructor above the
template-entry cap falling back; a multiple assignment using a wide-key
`SET_FIELD_AT`; more than 255 cache candidates of each kind executing through
sentinels; save/load round trips with high-byte indices and all three
sentinels; forged snapshots with out-of-range `Bx` literals or template keys
rejected.

Do not change the cost model. Determinism unaffected. The golden fixture will
need regenerating - the orchestrator does that.

### Superseded: the original deferral rationale

**String-pool expansion.** It is the first wall in three of the four repros,
but it is the most cross-cutting change available: globals, fields, string
values, table templates, runtime dispatch, the verifier and snapshots all
participate, and `GET_FIELD`/`GET_GLOBAL`/`SET_FIELD` have already spent their
remaining bits on caches and offsets.

**Number-pool expansion.** Narrower than strings but still needs a widened or
new operand meaning plus snapshot-verifier work. Keeping it separate allows an
explicit compatibility decision rather than silently reinterpreting old
`PUSH_NUM` words.

Both pools could technically go from 255 to 256 entries since index 255 is
representable (`parser.rs:289`, `734`; `compiler/verify.rs:182`). **Do not do
this** - one extra entry does not address the finding and would burn the
sentinel value.

**Therefore #44 is only partly fixed.** Update the finding to cover the two
remaining pool ceilings rather than deleting it, and note that the SET_FIELD
work does not fix the 300-distinct-field-name repro because the string pool
still fails first - it removes an independent ceiling for functions with many
assignments to *reused* field names.

### Tests

300 sequential array entries; a constructor mixing array and named/computed
fields across a batch boundary; a dynamic tail call after a flushed batch;
more than 255 SET_FIELD sites executing correctly through the uncached path;
saved bytecode carrying a sentinel loading successfully; the `num_locals` unit
test; and the flipped traceback line.

Cost neutrality: static analysis sums `A` counts over `SET_LIST`
(`lib.rs:138`) and the runtime charges the resolved element count
(`frame.rs:410`), so batching must not change either total.

Read `src/compiler/parser.rs` (`add_local`, `remove_instr`, call emission),
`src/compiler/parser/expr.rs`, `src/compiler.rs` (cache-slot assignment,
finalization), `src/compiler/verify.rs`, `src/instr.rs`,
`src/vm/eval_store.rs` (`instr_set_field`, `instr_set_list`), and
`src/vm/save_state/verify.rs`.
