# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #10, #11: a forged save file can abort the host process

Both are in `src/vm/save_state.rs` and both are hostile-input hardening on the
same code path, so they want one verifier pass rather than two.

Framing: game saves are user-editable files. The decoder already takes a
hostile-input posture - length caps, `read_exact`, reservations capped at
remaining bytes, graceful `LoadError::CorruptArena` / `DecodeError` - so the
gap is not "we forgot saves are untrusted", it is that the *byte* layer is
validated and the *semantic* layer is not.

Both verified by reading.

### #10 (High) - `load_state` performs zero bytecode validation

`build_bytecode` (`save_state.rs:658-710`) rebuilds a `Bytecode` directly out
of attacker-controlled fields. Nothing between the decoder and the interpreter
checks that any of it makes sense:

```rust
code: src.code.iter().map(|raw| Instr::from_raw(*raw)).collect(),
```

Every field is copied through as-is. Concrete abort vectors, all ordinary
safe-Rust panics rather than UB - but they kill the host process and are not
catchable as a `LoadError`:

- code that runs off the end (no trailing `OP_RETURN`): `Frame::get_instr`
  indexes `code[self.ip]` (`frame.rs:103-107`) - OOB panic.
- `OP_PUSH_NUM` / `OP_PUSH_STRING` / `OP_CLOSURE` with an index past
  `number_literals` / `string_literals` / `nested` (`frame.rs:110-117`,
  `eval_index.rs:371`) - OOB panic.
- `OP_GET_LOCAL` / `OP_SET_LOCAL` past the frame (`eval_index.rs:436-440`), or
  `OP_GET_UPVALUE n` on a chunk with no upvalues (`eval_index.rs:443`).
- `OP_GET_BUILTIN` / `OP_SET_BUILTIN` with a slot >= `Builtin::COUNT`
  (`eval_index.rs:414-423`).
- `OP_POP` / `OP_SWAP` / `OP_DUP` on an empty stack - the `pop_val` expect, or
  a swap OOB.
- `num_locals` / cache-slot counts inconsistent with the cache-indexed
  instructions that reference them.

**New since the parser work landed:** `line_info` is cloned at line 705 with no
check that its length equals `code.len()`. That equality is now a real
invariant - the parser maintains it and `finalize` carries a `debug_assert` for
it - so a forged save can hand the VM a chunk that violates an assumption the
rest of the code was just taught to rely on.

Also forgeable as plain data: `cost_remaining`, `cost_budget`, `cost_used` (a
save-file editor grants itself an unlimited tick budget) and `rng_state`. Those
are arguably acceptable, since saves are host-owned data and the host chooses
to load them - but the panic class is not, given what the decoder otherwise
promises.

### #11 (High) - `build_bytecode` recursion is unbounded

Same function. Recursion through `nested` chunk ids has cycle detection
(`visiting`, lines 667-673) but no depth bound:

```rust
for child in &src.nested {
    nested.push(build_bytecode(*child as usize, saved, out, visiting)?);
}
```

A forged save can encode N chunks in a linear parent-child chain at a few dozen
bytes each, so a ~10MB file yields recursion hundreds of thousands of frames
deep: native stack overflow, an abort, not a `LoadError`. Legitimate saves are
bounded by the compiler's own nesting limits, so a depth cap costs real data
nothing.

Note loop 4 added `MAX_SYNTAX_DEPTH = 200` on the parser side, which bounds how
deeply a *legitimately compiled* chunk can nest. That gives a principled number
to reuse here rather than inventing one.

### Corrections to the above, from review

- A forged `nested` chunk id panics during `load_state` itself: `build_bytecode`
  indexes `out[idx]` at line 664 *before* checking `idx`.
- Jump targets must be in `0..code.len()`, **exclusive**. `Frame::jump` accepts
  `code.len()`, and the next `get_instr` then indexes that position and panics.
- `line_info.len() != code.len()` does **not** currently panic - the runtime
  lookup is `.get(...).unwrap_or(0)`. Still reject it, as a declared invariant.
- Out-of-range cache operands do **not** currently panic; all three cache
  vectors use `.get()`. Forged cache *counts* still matter because they drive
  allocations in `compiler.rs:322`.
- Unknown opcodes, most out-of-range jumps, and invalid outer table-template
  ids already produce errors rather than aborts. Validate them anyway, but they
  are not current abort vectors.

---

## Agreed implementation plan - phase 1 only

The full verifier the review specified includes an operand-stack dataflow
analysis: abstract stack heights, the two auxiliary marker stacks
(`vararg_call_bases`, `table_constructor_bases`), dynamic result counts, and
agreement at control-flow joins. That is a JVM-style bytecode verifier.

**It is deliberately split out of this loop.** The structural and index checks
each reject a program the compiler provably cannot emit, so their
false-positive risk is near zero. The dataflow model's is not: if it is wrong
anywhere, or merely more conservative than the compiler in one corner, valid
saves produced by this very build stop loading. For a shipped game that
destroys player data, which is a worse failure than the abort being fixed.

Phase 2 is tracked as a new finding in `notes/bugs.md` rather than dropped.

### What phase 1 covers

Per `SavedBytecode`:

- `code` non-empty; last instruction is `OP_RETURN` (unreachable earlier
  returns are fine); every opcode known; reserved operand bytes zero for the
  opcode's encoding form.
- `line_info.len() == code.len()`.
- Same-build pool ceilings: at most 255 number literals, string literals, table
  templates, nested chunks, upvalue descriptors, and entries per template.
- `num_params + num_locals` computed without overflow and <= 255.
- Every nested chunk id exists - **checked before `build_bytecode` runs**,
  which is what closes the `out[idx]` panic.
- The nested graph is acyclic, and its longest parent-to-child path is <= 200.
- Per parent-child edge: `UpvalueDesc::Local(i)` requires
  `i < parent.num_params + parent.num_locals`; `UpvalueDesc::Upvalue(i)`
  requires `i < parent.upvalues.len()`.

Per saved closure object:

- Its chunk id and every saved upvalue id exist.
- `saved_closure.upvalues.len() == referenced_chunk.upvalues.len()`. Without
  this a chunk can satisfy the descriptor-count check yet be instantiated with
  fewer captured values and panic later in `GET_UPVALUE`.

Per instruction, operand bounds:

| Instruction | Check |
|---|---|
| `PUSH_NUM A` | `A < number_literals.len()` |
| `PUSH_STRING A` | `A < string_literals.len()` |
| `GET_GLOBAL A` / `SET_GLOBAL A` | valid string index; bytes are UTF-8 |
| `GET_FIELD A` | valid string index |
| `SET_FIELD A,B,C` | `B` valid string index; `C` valid set-field cache slot |
| `INIT_FIELD A,B` | `B` valid string index |
| `INIT_FIELD_PINNED A,B` | `A` valid string index |
| `NEW_TABLE_TEMPLATE A` | `A < table_templates.len()` |
| every template key byte | valid string-literal index |
| `CLOSURE A` | `A < nested.len()` |
| `GET/SET_BUILTIN A` | `A < Builtin::COUNT` |
| `GET/SET_LOCAL A` | `A < num_params + num_locals` |
| `GET/SET_UPVALUE A` | `A < upvalues.len()` |
| `FOR_PREP/FOR_LOOP A` | `A..A+4` fits the frame slots |
| `TFOR_PREP A` | `A..A+3` fits |
| `TFOR_CALL A,B` | `B >= 1`; `A..A+3+B` fits |
| `TFOR_LOOP A` | `A..A+4` fits |
| `CLOSE_UPVALUES A` | `A <= frame slot count` |
| `VARARG` | only when `is_vararg` |
| `PUSH_BOOL A` | `A` is 0 or 1 |
| `CONCAT A` | `A >= 2` |
| `MARK_CALL_BASE A` | 0 or 1 |

Jump targets: `target = pc + 1 + signed_offset` with checked arithmetic,
require `target < code.len()`.

Cache slots are **recomputed exactly**, not merely bounds-checked against
attacker-declared lengths: dense slots per distinct `GET_GLOBAL` string id in
first-use order; one dense sequential slot per `GET_FIELD`; one per
`SET_FIELD`. Declared counts and encoded indices must equal the recomputed
values. This is what stops a forged cache count driving an unjustified
allocation.

### Structure

A private `VerifiedSavePayload(SavePayload)` with no unchecked constructor.
Flow: decode bytes -> verify -> `VerifiedSavePayload` -> construct State/setup
-> materialize. `materialize_payload` and `materialize_bytecode` accept only
the verified wrapper, so "validated" is a type-level property rather than a
convention. Implementation in `src/vm/save_state/verify.rs`.

Verification runs **before** constructing the standard environment or
allocating runtime caches. Use `Vec`, fixed `[Option<u16>; 256]` / `[bool; 256]`
arrays, and `VecDeque`, processing chunk ids in ascending order: no banned hash
collections, deterministic first-error reporting, and scratch storage
proportional to decoded data rather than to any forged count.

### Producer-side validation (the false-positive safeguard)

Factor the checks over a small read-only bytecode view implemented by **both**
`Bytecode` and `SavedBytecode` - one implementation, no duplicated operand
tables, no adapter that reinterprets operands differently.

Run it on every compiler-produced chunk in `finalize`, after cache assignment,
as a `debug_assert`-level check. Then the parser tests, integration tests,
examples corpus and differential gate continuously prove that compiler output
satisfies exactly what the loader demands. This is the mechanism that will make
phase 2 safe to land later, so it is worth building now even though phase 1's
checks are low-risk on their own.

### #11: depth bound

Expose the parser's 200 constant (`parser.rs:18`) as a shared crate-private
limit rather than inventing a second number. Root chunk depth is 1; reject any
graph whose longest parent-to-child path exceeds it. Compute depth and detect
cycles iteratively with a topological worklist. `build_bytecode` stays
recursive behind `VerifiedSavePayload`, its native recursion now bounded at
200.

Not worth also rewriting materialization as an iterative post-order here. The
verifier's worklist already removes the hostile recursion. And #27 (GC mark)
and #28 (`encode_object`) do **not** genuinely share an implementation with
this - they share a design pattern only, working on `ObjectPtr` reachability
and on allocate/fill phases respectively, not on integer chunk ids.

### Error type

New variant, rather than overloading `CorruptArena`:

```rust
LoadError::InvalidBytecode {
    chunk: u32,
    instruction: Option<u32>,
    reason: String,
}
```

Split of responsibilities: `DecodeError` for malformed binary encoding;
`CorruptArena` for nonexistent string/object/upvalue/chunk arena ids;
`InvalidBytecode` for known ids arranged into an invalid program. Traversal and
reason text must be deterministic; tests match the variant, not the wording.

### Honest statement of what remains

Phase 1 plus #11 does **not** establish "no malformed save can abort during
load". Two gaps remain, and both should be stated plainly rather than papered
over:

1. **Stack discipline.** Forged bytecode can still underflow, and the panic
   sites are broader than `pop_val`'s expect: `DUP`'s `.last().expect`, `SWAP`'s
   `len - 1`/`len - 2`, `CONCAT A`'s `len - A` (`eval.rs:235`),
   `MARK_CALL_BASE`, table init, field/table stores, fixed `SET_LIST`,
   `GET_TABLE`'s `len - 1`, fixed `RETURN n` while locating return values
   (`eval.rs:434`), `RETURN RetCount::All`'s `stack.len() - frame_base`
   (`eval.rs:407`), direct `GET_LOCAL`/`SET_LOCAL` access, numeric and generic
   loop-local ranges, a later `get_top()` below `stack_bottom`, and open
   upvalues indexing frame slots that malformed code already popped. This is
   phase 2.
2. **Recursive GC.** `materialize_payload` calls `gc_collect()`
   (`save_state.rs:601`), so a decoded save containing a deep but entirely
   *legitimate* table graph can still overflow via #27. Rejecting deep object
   graphs is not an option - scripts build them normally - so this needs #27's
   iterative marking.

The defensible promise once all three land is narrow, and should be worded as
such: *malformed save structure is rejected with a `LoadError`; it cannot
trigger an indexing, stack-underflow, or recursive-traversal panic during
load.* Allocation failure and documented infinite loops are out of scope.

### Cost and RNG forgeability

RNG state needs no validation - every `u64` is valid SplitMix64 state. Document
that save authentication is required if changing future random outcomes
matters.

Cost is different: the budget is host sandbox policy, and a verifier cannot
distinguish a genuine budget triple from a consistently forged one. Do not mix
an API change into this loop. Document now that hosts loading user-editable
saves must call `set_cost_budget` after a successful load; a `LoadOptions`
policy API is a possible follow-up, with today's restoring behaviour as the
compatibility default.

### Tests

Private helpers in `save_state.rs` that: build and run a small valid Lua
program, save it, decode to `SavePayload`, mutate one field, re-encode with the
normal encoder, and assert the expected `LoadError`.

Fixtures: invalid nested chunk id (the current direct load panic); empty code;
missing final return; jump to `code.len()`; negative and out-of-range jumps;
number / string / nested / builtin / template / template-key indices; cache
count and operand mismatches; local and upvalue operands; closure capture-count
mismatch; child `Local` and `Upvalue` descriptors invalid for their parent;
`line_info` mismatch; unknown opcode; a self-cycle and a two-node cycle; a
programmatically built 201-chunk chain rejected, plus a 200-depth boundary
fixture that loads.

Plus: a representative compiler-produced save exercising nested closures,
captures, loops, caches, dynamic calls and dynamic table constructors that must
load successfully, and a load-then-resave byte-equality test. Keep the existing
corruption sweep but do not treat it as a substitute - it generally does not
invoke loaded closures.

---

## Review findings to fix

Phase 1's core is implemented correctly - nested-id checking before
materialization, exclusive jump bounds, exact cache recomputation, the
`VerifiedSavePayload` type gate, depth and cycle checking, compiler-side
validation of every recursively finalized chunk, and no phase-2 dataflow. These
five items remain.

### 1. FALSE POSITIVE, severe: valid binary string literals are rejected

`check_utf8_operands` (`src/vm/save_state/verify.rs:79`) requires `PUSH_STRING`
operands to be UTF-8. **Lua strings are byte strings.** The compiler emits
non-UTF-8 literals deliberately, and already has a test for it
(`src/compiler/parser/tests.rs:58`). So this same-build program saves fine and
then fails to load:

```lua
function f()
    return "\255"
end
```

This is precisely the failure mode the phased split exists to avoid: a verifier
rejecting a legitimate save destroys player data.

It also breaks the one-shared-implementation rule. The compiler debug assertion
calls only `validate_bytecode`, while this saved-bytecode-only path imposes
extra operand semantics that producer-side validation never exercises - which
is why the corpus did not catch it.

Fix: require UTF-8 for `GET_GLOBAL` / `SET_GLOBAL` operands only (those really
are Rust `String` global names). Do **not** require it for `PUSH_STRING`,
`GET_FIELD`, `SET_FIELD`, `INIT_FIELD`, `INIT_FIELD_PINNED`, or table-template
key bytes. Then make sure the shared view is genuinely the single source of
opcode operand rules, so producer-side validation covers whatever the loader
enforces.

Add a test: a script containing `"\255"` must save and load and still compare
equal.

### 2. The same aliasing bug remains in the upvalue arena

`encode_upvalue` (`save_state.rs:375-389`) records an id before recursively
encoding its value and pushes the arena entry afterwards - identical in shape
to the bytecode bug just fixed. Reachable:

```lua
function make()
    local v = 7
    local holder
    local function a() return holder end
    local function b() return v end
    holder = { b = b }
    return a
end
root = make()
```

Encoding `root` starts upvalue `holder` as id 0; its table reaches `b`, whose
distinct `v` upvalue is also assigned id 0 because the arena is still empty.
Both record id 0, and after a load `root()` returns `7` instead of the table.

Phase 1's verifier accepts this, because every id and capture arity is
structurally valid. Apply the same reserve-the-slot-before-recursing fix.

### 3. Writer regression tests, independent of the verifier

An aliased arena is structurally valid, so the verifier cannot be the test for
either aliasing bug. `deeply_nested_closures_round_trip`
(`tests/save_state.rs:667`) does not catch the bytecode one because it only
invokes leaf closures that already existed; it never calls the saved `outer`.

Add two tests that would fail without the writer fixes:

- save `outer`, reload, then **call `outer`** to create and invoke a *new*
  child closure;
- the `make`/`holder` program above: reload and assert `root()` returns the
  table, not `7`.

### 4. Reserved-operand validation is incomplete

The default branch (`src/compiler/verify.rs:233`) checks only `B == 0 && C == 0`,
so genuinely operandless instructions - `POP`, `DUP`, `SWAP`, the arithmetic
ops, `PUSH_NIL` - still accept a forged nonzero `A`. Not an abort vector, but it
fails the stated "all reserved operand bytes are zero" invariant. Classify
opcodes by encoding form and check every unused byte.

### 5. Finding #59's inventory is incomplete

The deferred-phase-2 finding omits panic paths that open up once malformed
bytecode has popped *below the frame's declared slots*:

- `GET_LOCAL` / `SET_LOCAL` index the shortened stack directly
  (`eval_index.rs:436`, `eval_store.rs:454`);
- the numeric and generic loop helpers index their declared local ranges after
  the same erosion;
- `RETURN RetCount::All` underflows `stack.len() - frame_base` (`eval.rs:407`),
  not just fixed returns;
- a subsequent call can then panic in `get_top()` when
  `stack.len() < stack_bottom`.

### 6. Most of the agreed test matrix is missing

Implemented fixtures do assert real `LoadError` variants and exercise the
fields they claim. But still absent: binary/string operands, nested opcode
indices, builtin / template / template-key indices, local and upvalue operands,
declared cache-count corruption, invalid child descriptors, `line_info`
mismatch, unknown opcodes, negative jumps, a two-node cycle, and the
representative compiler-produced load-then-resave byte-equality test.

### Note on save compatibility

The bytecode fix changes bytes only for the first-seen-parent nested case,
which the old writer serialized *incorrectly* - those saves were already
corrupt. Saves the old writer got right (flat chunks, and nested parents whose
children were all encoded earlier) keep identical ids, ordering and bytes. So
this is **not** a compatibility break for previously correct saves, and
`FORMAT_VERSION` does not need to bump. Existing corrupt saves are not
repaired: they will either load the wrong closure as before, or now be rejected
when the aliased chunk's capture arity happens to differ.
