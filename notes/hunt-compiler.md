# Hygiene hunt: compiler scope

Scope: `src/compiler.rs`, `src/compiler/` (lexer, token, parser and submodules,
exp_desc, verify), `src/numeral.rs`, `src/instr.rs`, plus every place the VM,
stdlib, cost analyser or snapshot code keeps its own copy of a rule the
compiler owns. Not ranked. Each item says what would hold the fix in place.

---

## A. The opcode set has no single owner (structural, the headline)

`src/instr.rs` defines each opcode as a bare `pub(crate) const OP_X: u8`. Every
other fact about an opcode is written out by hand somewhere else, and each
copy is a full list that has to be updated when an opcode is added:

| Fact about an opcode | Where it is spelled |
|---|---|
| number | `instr.rs` consts |
| is it a known opcode | `verify.rs` `known_opcode` (a 60-arm `matches!`) |
| which operand bytes must be zero | `verify.rs` `reserved_zero`, as three hand-kept groups plus a nested "A is reserved" list |
| operand-stack effect | `verify.rs` `verify_stack_discipline` |
| is it a control transfer, where does it go | `verify.rs` successor match, `verify.rs` jump-target range check (a second list in the same file), `compiler.rs` `transfer_with_offset`, the VM dispatch in `vm/frame.rs` |
| display name | `instr.rs` `impl Debug` |
| cost class | `lib.rs` `ScopeCost::analyze_chunk` (static) and the `add_cost!` sites in `vm/frame.rs` (runtime) |
| constructor shape | the `Instr::xxx()` helpers |

The lists have already drifted in small ways:

- **Debug drops operands.** It prints `Concat` with no count and
  `MarkCallBase` with no adjustment, so a `debug_parser` dump of a chunk loses
  information the verifier depends on.
- **The strip-remap test misses the fused branches.** The `transfer` fixture
  in `compiler.rs`'s tests (`strip_remaps_every_transfer_forward_and_backward`)
  lists 7 transfer opcodes. `transfer_with_offset` handles 13: the six fused
  `BRANCH_FALSE_*` opcodes (64-69) are missing from the test. The remap of a
  fused branch over a stripped NOP is only covered indirectly, through the
  example corpus.
- **The dispatch depends on opcode numbering.**
  `Instr::OP_BRANCH_FALSE_LESS..=Instr::OP_BRANCH_FALSE_NOT_EQUAL` in
  `vm/frame.rs` only works because 64-69 are contiguous and nothing else sits
  between them. The fallback arm in `eval_control.rs` `instr_branch_fused`
  turns a violation into a runtime `InternalError`, not a build failure.
  Nothing guards the numbering.
- **Static and runtime cost are two copies of one model.**
  `analyze_chunk`'s cost table (arith, negate, four new-table forms, six
  table-write forms, SET_LIST) and the runtime `add_cost!` sites restate the
  same rule. The only thing keeping them in step is
  `stripping_preserves_costs...` and the few `lib.rs` cost tests.

**Fix.** Generate every per-opcode fact from one declarative table: a
`macro_rules!` that takes `(NAME, number, operand shape, stack effect,
transfer kind, cost class, debug format)`. It emits the consts, `known_opcode`,
the reserved-byte mask, `is_transfer`/`with_offset`, the `Debug` impl and the
static cost class. The verifier's stack-effect match can stay hand-written for
the dynamic opcodes and read the table for the fixed ones. **Enforceable:** a
generated exhaustive table makes a missing entry a compile error. Add a
`const _: () = assert!(...)` that 64..=69 is exactly the fused-branch group,
and drive the strip test fixture from `is_transfer` over all 256 byte values.

## B. Sentinel encodings that alias a legal value

1. **`RetCount::Fixed(255)` silently means `All`.** Same for
   `ArgCount::Fixed(255)` and `Dynamic`. `to_u8` maps `Fixed(n)` to `n`, so
   `Fixed(255)` encodes to the sentinel byte. Several compiler sites depend on
   this aliasing on purpose or by accident:
   - `parse_explist` allows 255 expressions, and `parse_return` then emits
     `Instr::ret(RetCount::Fixed(n))` with `n` = 255. `return e1, ..., e255`
     therefore compiles to "return all". It happens to give the same answer
     because the operand stack holds exactly those 255 values at a statement
     boundary. Fixed returns and the runtime `RetCount::All` path ("too many
     results (limit 255)" in `vm/eval.rs`) are not the same rule.
   - `expr.rs` (both the call and method-call tails), `stmt.rs` `parse_return`
     and `adjust_multi_assign` rebuild an inner call as
     `ArgCount::Fixed(call.num_args())` / `Fixed(old.a())`. When the inner
     call was itself dynamic (`num_args` = 255), the result only stays dynamic
     because `Fixed(255)` encodes to `Dynamic`. `table.rs` instead uses
     `ArgCount::from_u8(instr.a())`, which is the correct spelling of the same
     operation. Same rule, two spellings, one of which is right by coincidence.
   - `CallSite` stores `num_args: u8` where 255 means dynamic, with no type
     to say so.

   **Fix.** Make the bad value unrepresentable: `Fixed` holds a newtype whose
   constructor rejects 255, `CallSite` stores an `ArgCount`, and
   `parse_explist`/`parse_return` check against the fixed-count limit. **Enforceable** by type.
   A test that `return` of 255 literals produces `ret(Fixed)` or a syntax
   error would also catch it.

2. **"Uncached" has three encodings.** In `instr.rs` and `assign_cache_slots`:
   - `GET_GLOBAL` and `GET_FIELD`/`SET_FIELD` use 255 for "uncached".
   - `SET_GLOBAL` and `TFOR_CALL` use a +1 bias with 0 for "uncached".

   The bias is justified as "including legacy snapshots". `save_state.rs`
   rejects any snapshot whose version differs from `FORMAT_VERSION`, with no
   fallback (CHANGELOG 0.4.0), so no legacy snapshot can reach this code.
   The compatibility reason no longer holds, and the split is now pure
   inconsistency. The verifier re-encodes each rule separately
   (`a != u8::MAX`, `a != 0`, `a as usize - 1`). **Fix:** pick one encoding,
   behind a `CacheSlot` newtype with `none()`/`slot(i)` accessors used by the
   compiler, the verifier and the VM. **Enforceable** by type.

3. **`mark_call_base(adjustment)` has one legal value.** The verifier rejects
   anything except `a == 1`, and the compiler only ever passes 1. The VM still
   reads `adjustment` and computes with it. The operand is a switch with one
   value since it was added. Either drop the operand, or have the VM treat it
   as the constant the verifier says it is.

## C. Limits: one value, several owners

- **`MAX_SYNTAX_DEPTH` carries three meanings.** It bounds:
  - parser recursion (`parser.rs`);
  - the verifier's call-base and table-marker stacks (`verify.rs`, "marker
    stack is too deep");
  - snapshot nested-chunk depth (`vm/save_state/verify.rs`).

  The second and third only hold because the parser happens to spend at least
  one syntax level per marker and per nested function. Each paren spends two
  (the parser test notes an effective paren limit of about 99). If the parser
  ever charges fewer levels, the verifier will start rejecting the compiler's
  own output. If it charges more, the verifier limits become looser than the
  compiler's real limits, which is harmless. Give each meaning its own named
  constant, derived from the first with a comment saying why. **Enforceable:**
  a test that compiles the deepest accepted program and checks the marker
  depth it reaches against the verifier constant.
- **The 255/254 limits live as bare literals in many places.**
  - Parser: `add_local` (`u8::MAX`), `checked_fixed_arg_count` (`254`),
    `add_upvalue`/`checked_parent_upvalue_index` (`u8::MAX`),
    `parse_fndef_named` and `parse_fndef_method` (the nested-function cap,
    once each), `try_use_table_template` (`u8::MAX`, twice), table batches
    (`i == u8::MAX`).
  - Verifier: `validate_bytecode` restates them (`len > 255`,
    `> usize::from(u16::MAX) + 1`, `num_params + num_locals > 255`).
  - Error text in `error.rs` hard-codes them again: `"(limit 255)"`,
    `"(limit 254)"`.
  - The README repeats them ("255 locals and 255 upvalues, and 65536 ...").

  **They have already diverged:** `TooManyExpressions` says "limit 255", but
  `checked_multi_assign_width` raises it for a width of exactly 255, so the
  real limit there is 254. The same variant also covers concat chains over
  255 and the `set_field_at`/`set_table` offset overflow, so one message
  describes three different limits. **Fix:** a `limits` module (`MAX_LOCALS`,
  `MAX_UPVALUES`, `MAX_FIXED_ARGS`, `MAX_NESTED`, `MAX_LITERALS`, ...). The
  parser, the verifier and the error `Display` impls interpolate those
  constants. **Enforceable:** one test per constant that compiles N and N+1
  and checks the message names N.
- **`Builtin::COUNT = 19` is a hand-kept literal, and an `unsafe transmute`
  depends on it.** `Builtin::from_u8` is sound only if `COUNT` equals the
  number of variants and the discriminants are dense. Bump `COUNT` without
  adding a variant and the transmute becomes UB on a value that forged
  snapshot bytecode can supply (the verifier checks only `a < COUNT`).
  `from_name` and `name` are two more hand-kept tables, and nothing tests the
  round trip. **Fix:** generate the enum, `COUNT`, `name` and `from_name`
  from one list in a macro, and make `from_u8` a safe lookup into a
  `[Builtin; COUNT]` array. **Enforceable** by construction. Until then, a
  test that `from_u8(i).map(name).and_then(from_name) == from_u8(i)` for
  every `i` in 0..=255. Lateral: `vm.rs` reserves `Builtin::COUNT + 4`, a
  bare magic number.
- **Opcode numbers and builtin slot indices are part of the save format, and
  nothing ties them to `FORMAT_VERSION`.** Snapshots store raw `u32`
  instructions and builtin slots by index. Renumbering either one changes
  what an existing snapshot means without moving `FORMAT_VERSION`. The fused
  branch CHANGELOG entry relies on "unknown opcode" rejection, which only
  covers additions, not renumbering. **Enforceable:** a golden test that pins
  the opcode-number and builtin-index tables next to `FORMAT_VERSION`, so
  changing either fails until both are updated together.
- **The tfor-cursor slot count is derived twice.** `assign_cache_slots`
  counts it (`tfor_cursor_len`) and throws the count away.
  `RuntimeCaches::new` recomputes it as `max(C)` over the code. The comment
  explains why: a fourth count field would change the snapshot format. That
  is a legitimate reason for a second copy, and the verifier's sequential
  slot check is what keeps the two in step. The comment hard-codes "format
  6", which is already a value that drifts. Reword it to "the snapshot
  format".
- **The verifier restates the cache-slot allocation algorithm exactly.** It
  re-derives first-use order for globals and instruction order for fields
  and set-fields. That copy is a deliberate verifier and is held by the
  corpus test `compiler_examples_pass_stack_discipline_verification`. Note
  that any future change to the allocation policy (for example, sharing
  GET_FIELD slots by name) must change both places.
- **Two line-number widths.** `CallSite` saturates call lines at 2^24, while
  `line_info` is `u32`. In a source longer than 16M lines, a call's
  traceback line freezes while neighbouring instructions report the true
  line. Either use one width or use the same saturation everywhere.

## D. Rules implemented more than once inside the compiler

- **Numeral grammar has two owners.** `numeral.rs` says it is "shared by
  runtime-facing conversions and the compile-time constant folder", but only
  hex literals go through `parse_lua_numeral`. Decimal literals in
  `expr.rs` `parse_expr_base` call `str::parse::<f64>` directly, without
  `numeral.rs`'s `validate_mantissa`/`validate_signed_digits` pre-filter.
  This is safe today only because the lexer's acceptance grammar
  (`lex_full_number`, `lex_exponent`) filters first. The lexer is a third,
  separate spelling of the same grammar. Route both literal kinds through
  `parse_lua_numeral`. **Enforceable:** a property test that every token the
  lexer accepts as a number gives the same value through the parser as
  through `tonumber`.
- **`hex_value` is defined twice,** identically, in `parser.rs` and
  `numeral.rs`. Delete the parser copy.
- **The CR/LF-pair rule is written three times:** `lexer.rs` `next_char`,
  `lexer.rs` `lex_string`, and `parser.rs` `get_literal_string_contents`.
  Each uses its own `next != c && matches!(next, '\r' | '\n')` expression.
  Extract one `is_newline_pair(a, b)` into `numeral.rs` next to
  `is_lua_whitespace`.
- **The "numeral glued to identifier" check** (`is_ascii_alphanumeric() ||
  '_'`) appears twice in the lexer (hex path and `lex_exponent`). The
  identifier character class appears a third time in `lex_word`.
- **Keyword spellings live in two tables:** `lexer.rs` `keyword_match` and
  `parser.rs` `describe_token_type`. One `TokenType -> &'static str` table
  could drive both.
- **"Open the multi-value tail" has five implementations:** call args and
  method-call args in `expr.rs` (two near-identical 25-line blocks),
  `stmt.rs` `parse_return`, `stmt.rs` `adjust_multi_assign`, and `table.rs`.
  They use three different failure styles (`unreachable!`, `debug_assert!`,
  `debug_assert_eq!`) and two different ArgCount spellings (see B1). Write
  one `fn open_tail(&mut self, last: &ExpDesc, want: RetCount)`.
- **PlaceExp lowering is copied.** The get-instruction mapping is written
  three times (`parser.rs` `eval_prefix_exp`, `func.rs`
  `parse_fndecl_table` and `parse_fndecl_method`). The set-instruction
  mapping is written twice (`stmt.rs` `parse_assign`, `func.rs`
  `parse_fndecl_basic`). The `func.rs` copies end in `unreachable!`.
  `parse_fndef_named` and `parse_fndef_method` are the same 15 lines apart
  from one `params.insert(0, "self")`.
- **Each fold operator is spelled twice.** The parser pairs each opcode with
  its fold function (`parse_addition`, `parse_multiplication`, `parse_pow`),
  and `vm/frame.rs` pairs the same opcode with the same function again. The
  comment "must be the same function the VM dispatch applies" is the only
  guard. Add `fn arith_fn(opcode) -> fn(f64, f64) -> f64` in `numeral.rs`
  (or the opcode table) and call it from both. **Enforceable** by making it
  the only path.
- **The comparison-to-fused-branch mapping** is in `push_branch_false`. The
  ordering trick (`<=` is `!>`) is spelled in both `vm/frame.rs` and
  `vm/eval_control.rs`. That is VM scope, but the opcode table in A could
  own it.
- **Internal errors are built three ways:** `compiler.rs` `internal_error`
  (no location), `parser.rs` `parse_chunk` (`error_at(InternalError, 0)`,
  which renders as line 1 col 1, a false location), and `finalize` (inline
  `without_location`). Use the one helper everywhere.

## E. Errors that lose their subject

- **`finalize` drops the location of a verifier failure.** It reports
  `"compiler emitted invalid bytecode: {reason}"` but throws away
  `BytecodeValidationError.instruction` (the pc), the chunk's `name` and its
  `source`. A compiler bug reported from the field names no function and no
  instruction. Lateral: the `#[cfg_attr(not(feature = "snapshot"),
  allow(dead_code))]` on `reason` is stale, because `finalize` reads it in
  every build.
- **Syntax errors have no source name.** `Error` carries line and column
  only, and `parse_str_named` has the name but never attaches it. The
  operator gets `3:7: ...` with no file. Lateral (VM scope):
  `load_string_named` sets `self.current_source` before parsing, so a
  rejected load leaves `current_source` pointing at the failed chunk.
- **Operator-facing text is assembled in the parser** instead of by
  `error.rs`'s `Display`: `UnexpectedTok(format!("'{exp}' expected near
  {got}"))` and `UnexpectedTok("cannot use '...' outside a vararg
  function")`. The second should be its own `SyntaxError` variant.
- **Aborts on caller-controlled input.** These need a source larger than
  4 GiB; nothing in the API caps source size.
  - `Token.len` is `u32`, set by `(self.pos - tok_start) as u32`, and
    `tok_line` is `self.linebreaks.len() as u32`. Both wrap silently.
  - A wrapped `len` feeds `substring` (a slice that can land off a char
    boundary and panic) and `assert!(len >= 2)` in
    `get_literal_string_contents`.

  Either cap source length at the entry point with a typed error, or use
  `try_from` there. The `peek_equals` `panic!` arms and the `unreachable!`s
  are internal-only and fine.

## F. Unbounded work and growth

- **Compile work is uncharged and unbounded.** `find_or_add_string_bytes`
  and `find_or_add` are linear scans, so building the literal pools is
  quadratic: up to 65536 x 65536 / 2 byte-slice compares per function.
  `analyze_cost` and `load_string` charge nothing for parsing, and the
  source length has no cap. A host that loads untrusted scripts (fleet code)
  can be stalled at load time. Fix: an `IndexMap` keyed by bytes for the
  pools, plus a documented, configurable maximum source size.
- **Every nested function holds its own copy of the source name.**
  `parse_chunk` clones `source: Option<String>` into each nested chunk.
  Make it `Option<Arc<str>>`.

## G. Ambient and host assumptions

- **Constant folding of `^` puts the host's `libm` into bytecode.**
  `parse_pow` folds with `f64::powf`, which calls the platform math library.
  `powf` is not guaranteed bit-identical across targets or libm versions.
  The VM uses the same function at runtime, so results agree on a single
  host. But snapshots carry compiled bytecode, and a replay on another host
  then mixes a constant folded with host A's `powf` and runtime `powf` from
  host B. Replays need bit-exact determinism, so `pow` needs one owned
  implementation, for example a pure-Rust `libm`-style port, used by both
  the folder and the VM. At minimum, don't fold `^`. `+ - * / %` are IEEE
  exact and safe.
- **A compiler test depends on the working directory.**
  `compiler_examples_pass_stack_discipline_verification` walks
  `Path::new("examples")`, relative to the cwd. Use
  `env!("CARGO_MANIFEST_DIR")`.
- **A process-global compiled chunk.** `lua_std/string.rs` `GMATCH_WRAPPER`
  is a `static OnceLock<Arc<Bytecode>>`, compiled once per process and
  shared by every `State`. That is sound because `Bytecode` is immutable and
  the caches are per State. Its `source` is `None`, so tracebacks through
  `gmatch` name no chunk.

## H. Output channel

- **`debug_parser` writes to stdout from library code.**
  `println!("Compiled chunk: ...")` in `parse_chunk` prints the raw
  pre-finalize chunk (NOPs still present, cache slots unassigned), which is
  not the bytecode that ships. `debug_vm` does the same per instruction in
  `vm/frame.rs`. Neither feature is in brokkr's `[[check]]` sweep (only
  `default` and `snapshot` are), so both can stop compiling without anyone
  noticing. That is the same blind spot AGENTS.md describes for `hotpath`.
  Either route the dump through a host hook, or delete the feature and add
  a `Bytecode` disassembler test helper. **Enforceable:** add a `[[check]]`
  entry for each debug feature, or delete the features.
- **A trivial setter is instrumented.** `update_line` carries
  `#[hotpath::measure]` on a one-line setter called for every token, which
  inflates the parser's profile distribution.

## I. Cost-model ownership crosses into the compiler

- **`COST_MODEL_VERSION` did not move when the compiler changed costs.**
  The CHANGELOG Unreleased entry says constant folding lowers `cost_used`
  for identical source and asks users to re-baseline fingerprints.
  `COST_MODEL_VERSION` is still 2, and its doc comment describes only v1 and
  v2. The constant exists so hosts can identify the accounting model, and
  that model now changes whenever compiler emission changes. This has
  already happened once. **Enforceable:** a golden test pinning `cost_used`
  for a fixed corpus, with an assertion message naming
  `COST_MODEL_VERSION`, so any emission change that moves cost fails until
  the version is bumped deliberately.

## J. Code that is no longer load-bearing

- **`#[allow(dead_code)]` on `Instr::c()`** is stale. `c()` is used by the
  verifier, `RuntimeCaches::new` and the VM's `TFOR_CALL` dispatch. The lint
  gate denies `unused`, so with the allow removed the build would catch it
  if `c()` ever became unused.
- **Unused `_tok_start` parameters** on `peek_equals`, `lex_string` and
  `lex_exponent`, threaded through their callers.
- **`assign_cache_slots` returns `Result<()>` but has no error path.** The
  `?` in `finalize` reads as a check that never happens.
- **`strip_dead_instructions` re-checks `code.len() == line_info.len()`** as
  a runtime error, directly after `finalize`'s `debug_assert_eq!` on the
  same fact, and the verifier checks it a third time.
- **The "legacy snapshots" rationale** for the biased SET_GLOBAL/TFOR_CALL
  encodings (see B2). Strict version equality makes it dead.
- **`// TODO: rename to error_here`** on `Parser::error`, and the
  `parse_if` wrapper, which only forwards to `parse_if_arm`.
- **The verifier still accepts `OP_SET_FIELD_AT` with A=0, and `OP_NOP`.**
  The compiler never ships either: finalize rewrites offset-0 sites and
  strips NOPs. This is fine for hand-built snapshots, but say so in the
  verifier, or reject them so that "what the compiler emits" and "what the
  verifier accepts" stay one documented set.

## K. Documentation drift seen along the way

- **README: "bounds syntax nesting at 200 levels".** The effective limit for
  parenthesised expressions is about 99, because each paren spends two
  levels (noted in `parser/tests.rs`). Reword the README to describe the
  budget rather than a depth.
- **`instr.rs` module doc:** "4x smaller than the previous enum-based
  representation (~16 bytes per instruction)" is history about a
  representation that no longer exists. Drop it.
- **`frame.rs` `eval` doc: "Free operations: ... string operations".** This
  predates cost model v2, which charges native string work. That is VM
  scope, but it is the runtime copy of the cost table in A.
