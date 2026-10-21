# Hygiene hunt: compiler front end

Scope: `src/compiler.rs`, `src/compiler/**`, `src/numeral.rs`, `src/instr.rs`.
Values were followed across the boundary into `src/vm*`, `src/limits.rs`,
`src/vm/save_state*`, `tests/cost_golden.rs` and `docs/budget.md` where they lead
there. Findings are not ranked. Each one says whether the fixed version can be
enforced, and by what.

Existing enforcement read first: `Cargo.toml` `[lints.rust]`/`[lints.clippy]`
(no `panic`, `expect_used`, `unreachable`, `as_conversions` or
`cast_possible_truncation` lints), `clippy.toml` (host-state and
nondeterministic-collection bans, `Result::ok`), `src/lib.rs`
(`unwrap_used`, `float_cmp` outside tests, `missing_docs` as a warning), and the
golden fixtures (`tests/fixtures/opcode_encoding.txt`, `tests/cost_golden.rs`).
Those show the build can enforce text and lint rules, type-level rules, and
golden-file tests. It has no dependency-direction rule between modules.

---

## 1. One value, one owner

### 1.1 The snapshot format version is spelled twice, and one copy is checked only behind a feature
`src/vm/save_state.rs` has `const FORMAT_VERSION: u16 = 7`. `src/instr.rs`'s
`persisted_encodings_match_fixture` test hard-codes
`assert_eq!(version, "format=7")`, and the fixture file has a third copy on its
first line. The cross-check against the live constant runs only under
`#[cfg(feature = "snapshot")]`. In a default-feature `brokkr check`, the fixture
and `instr.rs` can agree with each other and both disagree with the writer.
- Fix: the fixture test reads `FORMAT_VERSION` (make it `pub(crate)`) instead of
  the literal, and compares it without the feature gate. The constant is plain
  data and does not depend on the snapshot feature being on.
- Enforceable: yes, by the test itself once it reads the constant.

### 1.2 The cache-slot allocation policy has three owners, and the operand encodings have four conventions
- `Bytecode::assign_cache_slots` (compiler.rs) allocates. `verify.rs` restates
  the policy, deliberately, as an independent check. `RuntimeCaches::new`
  recomputes the TFOR cursor count a third way (the maximum C operand), because
  storing a fourth count would change the snapshot format.
- The sentinel conventions differ per opcode, all of them persisted.
  `GET_GLOBAL`/`GET_FIELD`/`SET_FIELD` use 255 for "uncached".
  `SET_GLOBAL`/`TFOR_CALL` use 0 for "uncached" and bias slots by one.
  `SET_FIELD_AT` with A=0 means "not yet rewritten". Each rule is spelled in
  the `instr.rs` constructors, in `assign_cache_slots`, in the verifier, and in
  the VM decoding sites (`eval_store.rs`, `eval_index.rs`, `eval_control.rs`).
- The verifier copy is a duplication with a reason, and the comment in verify.rs
  says so. What keeps them in step is that `finalize` runs `validate_bytecode`
  on every compile. Drift fails every compile, not only the corpus test the
  comment names, so the comment undersells its own guard.
- The fourth copy, the VM decoding, has no checker. The fix is a typed operand
  decoder in `instr.rs`, e.g. `fn global_get_slot(self) -> Option<u8>` and
  `fn global_set_slot(self) -> Option<u8>`, paired with the encoders, so the
  bias and sentinel rules exist exactly once and the VM, verifier and finalizer
  all call them. At the next format bump, unify on one convention (biased, 0 =
  uncached, for every cached opcode) and store the TFOR count beside the other
  three. That retires `RuntimeCaches::new`'s recomputation.
- Enforceable: the decoders make the bad spelling unrepresentable outside
  `instr.rs` once `Instr::a()`/`c()` are no longer used for cache operands. A
  text rule (grep in `brokkr check`) for `inst.a()` near cache opcodes in `vm/`
  is weaker but possible.

### 1.3 Two opcode-to-function maps for arithmetic
`numeral::arith_fn` maps opcodes to `lua_add`...`lua_modulo` for the constant
folder. `vm/frame.rs`'s dispatch arms map the same opcodes to the same functions
by hand. The comment in `numeral.rs` says bit-exactness holds "by construction",
but that construction is two hand-written match tables that currently agree.
- Fix: make `arith_fn` a `const fn` and have each dispatch arm call
  `const { arith_fn(Instr::OP_ADD) }`. That adds no runtime lookup, and the
  folder and VM read one table. Better still, carry the function in the
  `define_opcodes!` metadata row.
- Enforceable: structurally, once dispatch reads `arith_fn`. A unit test that
  compares `arith_fn(op)` with each dispatch arm cannot be written, which is why
  the structural fix matters.

### 1.4 What counts as an "expandable tail" is spelled three times
`Parser::open_tail` (parser.rs), `adjust_multi_assign` (stmt.rs) and
`parse_table_entry` (table.rs) each match
`ExpDesc::Prefix(PrefixExp::FunctionCall(_)) | ExpDesc::Vararg` on their own.
`open_tail`'s `assert_eq!` and the two `assert!`s at its callers exist only
because the predicate lives in three places.
- Fix: `ExpDesc::is_multi(&self) -> bool`, or better an
  `ExpDesc::MultiTail(TailKind)` variant that `open_tail` takes by value, so a
  non-tail cannot reach it and the asserts go away.
- Enforceable: by type, if `open_tail` takes a `MultiTail` value.

### 1.5 The "check a limit, then `u8::try_from` with the same error" pattern appears at every limit
`checked_fixed_arg_count`, `checked_multi_assign_width`, `parse_concat`,
`parse_assign` (`MAX_ASSIGNMENT_OFFSET`), `parse_chunk` (params) and
`parse_generic_for` each compare against a `limits::` constant and then do a
`u8::try_from(..).map_err(same error)` that cannot fail. Elsewhere the code uses
bare `as u8` casts instead (`self.locals.len() as u8` five times in stmt.rs,
`local_idx as u8` and `upvalue_idx as u8` in upvalue.rs,
`self.chunk.nested.len() as u8 - 1`, `template_idx as u8`). Those are sound
today only because an earlier limit check bounds the value. Two styles for one
rule.
- Fix: a single `fn bounded_u8(value: usize, limit: usize, err: SyntaxError) ->
  Result<u8>` on `Parser`. Slot indices could also become a `Slot(u8)` newtype
  that only `add_local` constructs.
- Enforceable: `clippy::cast_possible_truncation` (and `as_conversions` scoped to
  `compiler/`) would flag every bare `as u8`. Neither is in `[lints.clippy]`
  today.

### 1.6 The table-presize threshold is spelled twice
`table.rs` uses `field_count > 4` in `parse_table` and `[TableTemplateField; 4]`
plus `self.inline.len()` inside `TableTemplateCandidate`. If one changes without
the other, `fields_for_template` silently returns `None` for 5..N fields, and
templates turn off for small tables without any test noticing.
- Fix: one `const INLINE_TEMPLATE_FIELDS: usize = 4`, used by both.
- Enforceable: by the const. A test pinning that a 5-field pure-named
  constructor gets a template would also catch it.

### 1.7 Number-literal recognition is split between the lexer and `numeral.rs`, with different acceptance
The lexer owns token boundaries and partly validates. Hex needs at least one
mantissa digit and at least one exponent digit, but a decimal exponent (`1e`,
`1e+`) is accepted by `lex_exponent` with zero digits and rejected later by
`numeral::parse_decimal`. The resulting error is the same (`BadNumber`), but the
rule "an exponent needs digits" is enforced in the lexer for hex and in numeral
for decimal. Hex-digit classification uses `char::is_ascii_hexdigit` in the
lexer and `numeral::hex_value` in the parser.
- Fix: the lexer only finds the maximal numeral span (Lua's `read_numeral`
  approach: digits, dots, exponent letters, signs after an exponent letter), and
  `numeral::parse_lua_numeral` is the single validator.
- Enforceable: by a test that runs every lexer number fixture through both
  paths. Not by lint.

### 1.8 String-literal bodies are scanned twice, and the newline-pair rule is restated
The lexer's `lex_string` scans the body to find the closing quote, with its own
handling of `\z` and escaped newline pairs. `Parser::get_literal_string_contents`
re-scans the same bytes to decode them, re-implementing `\z` and the
escaped-newline pair. `is_newline_pair` is shared, but the consumption rule
around it is written three times: in `Lexer::next_char`, `lex_string` and the
parser decode. Errors also split: the lexer reports unclosed strings, the parser
reports bad escapes.
- Fix: decode in the lexer, once, and hand the parser decoded bytes, either in
  the token or in a side buffer indexed by the token. The parser keeps no
  escape logic.
- Enforceable: structurally, since there would be one scanner. Today only the
  test suite keeps the two in step.

### 1.9 Three nesting counters
`Parser.nest_level: i32` (block scoping), `syntax_depth: u32` (recursion budget)
and `outer_funcs.len()` (chunk nesting) are three depth counters with three
types. `nest_level` is never negative but is signed. The `(name, i32)` local
tuples store it a second time per local.
- Fix: `nest_level: u32`, or better a block-start index stack so locals need no
  per-entry level at all (pop to the saved length, as reference Lua does).
- Enforceable: by type.

### 1.10 `MAX_STRING_BYTES` is reached through three paths
It is defined in `limits.rs`, re-exported from `vm.rs`, then re-exported from
`lib.rs` via `vm`. The parser uses `crate::vm::MAX_STRING_BYTES`,
`crate::vm::check_string_size` and `crate::vm::checked_string_growth`. The value
has one owner, but the policy (the check functions) lives in `vm`, so the
compiler depends on the VM for a limit check.
- Fix: move `check_string_size` and `checked_string_growth` beside the constant
  in `limits.rs` (or a small `strings` module) and import from there.
- Enforceable: only by a dependency rule (see 6.1).

### 1.11 `MAX_SOURCE_BYTES` silently underwrites three narrow widths
Three narrowing choices in scope are sound only because
`MAX_SOURCE_BYTES = 16 MiB`:
- the lexer's `tok_line = self.linebreaks.len() as u32` and `len: u32`;
- `CallSite`'s 24-bit line field (`LINE_MASK`);
- `Token.len: u32`.

Each site has a prose comment saying so, but nothing ties them together. Raise
the source cap above 2^24 and `CallSite` saturates line numbers in tracebacks
without any test failing. `MAX_SOURCE_BYTES` also equals `MAX_STRING_BYTES` by
coincidence (both `16 * 1024 * 1024`, spelled separately). `load` of a
max-length string is therefore accepted by the string cap and the source cap at
once. If one moves, `load` starts refusing strings the VM happily built, with
`SourceSizeExceeded`.
- Fix: `const _: () = assert!(MAX_SOURCE_BYTES <= CallSite::LINE_MASK as usize);`
  and a `u32::MAX` assert, both in `limits.rs` or next to the narrow fields.
  Decide explicitly whether `MAX_SOURCE_BYTES` should be defined as
  `MAX_STRING_BYTES`.
- Enforceable: yes, by compile-time asserts.

## 2. Values nobody can find, change, or trust

### 2.1 Tunables defined where they were first needed
The front end has tunables outside `limits.rs`:
- the presize/template threshold `4` (table.rs);
- the double syntax-level charge per call/index suffix (expr.rs,
  `enter_syntax_level` twice, justified by debug-build frame size);
- `MAX_SIGNIFICANT_HEX_DIGITS = 16` and the `+2048` exponent clamp (numeral.rs).

The second one is effectively a hidden multiplier on `MAX_SYNTAX_DEPTH`. Nested
calls get half the depth that nested parentheses get, and nothing in `limits.rs`
or `docs/limits.md` says so. Those are the places someone tuning the limits
would look.
- Fix: `limits.rs` gains `SYNTAX_COST_SUFFIX: u32 = 2` (or the parser exposes a
  `SyntaxCost` enum), and `docs/limits.md` documents that call and index nesting
  costs two units. The hex constants are numeric-correctness parameters, not
  tunables, and can stay, but should be documented as such.
- Enforceable: the `deepest_nested_programs_fit_verifier_marker_limit` test
  already depends on this ratio. Naming the constant lets that test read it
  instead of rediscovering it.

### 2.2 The constant-folding rules are a cost-model input documented nowhere a designer reads
Folding `PushNum; PushNum; Add` (and `Negate` of a literal) removes an
`Arithmetic`/`Negation` charge. So `x = 60 * 1000` costs 0, while
`local k = 60; x = k * 1000` costs 1, and `2 ^ 10` costs 1 because pow is never
folded. `docs/budget.md` says "naming an intermediate or any other legible
refactor must not make a script more expensive", and folding contradicts it
exactly. Only `tests/cost_golden.rs` pins this (`folded_literals`), and the only
doc mention is `docs/determinism.md`'s note about `^`.
- Either document folding in `docs/budget.md` as a deliberate exception (free
  literal arithmetic), or make folded expressions charge what they would have
  charged. The second option would need a per-chunk static "folded units" count
  charged on entry, which is awkward.
- Enforceable: the golden test enforces the number, not the documentation.

### 2.3 Folding leaves dead literal-pool entries
Folding `60 * 1000 + -5` leaves `60`, `1000`, `5`, `-5` and `60000` in
`number_literals`, but only the final result is referenced. They count toward
`MAX_LITERALS`, they are serialized into snapshots, and they become
`BytecodeRuntime.literals` slots per State. Harmless, but it is pool contents
that depend on how folding happened.
- Fix: compact the pool in `finalize` (remap `PUSH_NUM` Bx). That changes the
  snapshot contents of identical source, so it needs a note but not a format
  bump.

## 3. One channel, one implementation

### 3.1 `debug_parser` prints straight to stdout from library code
`parser.rs` `parse_chunk`: `#[cfg(feature = "debug_parser")] println!("Compiled
chunk: {tmp_chunk:#?}")`. The same pattern exists in `vm/frame.rs` (`debug_vm`)
and `vm/object.rs` (`debug_gc`). An embedding host that turns on a debug feature
gets output on the game's stdout, interleaved with `print`, which itself goes
through `HostCallbacks` (`host.rs`). The project has an output channel, the
callbacks, and the debug features bypass it.
- Fix: route debug output through a `HostCallbacks::debug(&str)` hook (default
  no-op, or stderr), or drop these features in favour of the `hotpath`
  instrumentation and a `--dump-bytecode` CLI flag in `main.rs`, the host.
- Enforceable: `clippy::print_stdout` and `print_stderr` denied in `lib.rs`
  under `cfg(not(test))`, with `host.rs`'s default `print` as the one `allow`.

### 3.2 `BytecodeValidationError` is rendered into text at two sites
`compiler::finalize` builds "compiler emitted invalid bytecode in function 'x' at
instruction N: reason" by hand. `save_state/verify.rs` builds its own
`invalid(chunk, instruction, reason)`. Same error type, two renderings, and
different subjects (function name in one, chunk index in the other).
- Fix: `impl Display for BytecodeValidationError` (with an optional chunk label)
  owned by `verify.rs`.
- Enforceable: by structure only.

### 3.3 `finalize` reports metadata failures at line 0, column 0
`validation_error.instruction == None` gives `unwrap_or(0)` and
`with_location(line_num, 0)`. Line 0 and column 0 are not "no location". The
error type has `without_location`, which is the correct spelling.
- Fix: `without_location` plus source name when there is no instruction.

## 4. Errors

### 4.1 Internal invariants panic in the parser, while `finalize` deliberately converts the same class of failure into an error
`finalize`'s long comment states the crate's stance: a compiler bug must surface
as a catchable `InternalError`, never as a host panic, "which is precisely the
failure mode this crate refuses". The parser does not follow it. These all abort
the host on a compiler bug:
- `assert_eq!` in `get_literal_string_contents` and `open_tail`;
- `assert!` in `adjust_multi_assign` and `parse_table`;
- `assert_eq!(self.nest_level, 0)` in `parse_all`;
- `panic!` twice in the lexer's `peek_equals`;
- `unreachable!` in `parse_fndecl_basic` and `numeral::arith_fn`;
- `expect` in `exit_loop`, `add_break`, `replace_last_instr`, `finalize`
  (`Arc::get_mut`) and `strip_dead_instructions`;
- `ArgCount::to_u8`/`RetCount::to_u8` asserts reached from `Instr::call`/`ret`.

None of these can be reached from script input today, so this is not a
user-controlled abort. It is one rule ("a compiler bug is an error") implemented
at one site and contradicted at roughly fifteen.
- Fix: decide the rule. Either make these `debug_assert!` plus an
  `InternalError` return (the `finalize` pattern), or delete `finalize`'s
  argument. Several disappear structurally: `ExpDesc::MultiTail` (1.4) removes
  three, a typed `CmpTokenKind` passed to `peek_equals` removes the panics, and
  `PlaceExp` split into `VarPlace`/`IndexPlace` removes the `unreachable!`.
- Enforceable: `clippy::panic`, `clippy::unreachable`,
  `clippy::missing_panics_doc` and `clippy::expect_used` scoped to `compiler/`
  via `#![deny(...)]` in `compiler.rs` (test modules allowed).

### 4.2 Fold failures are swallowed on purpose. Fine, but the policy is spelled twice
`push_arith` and `push_negate` both do `if let Ok(idx) = self.find_or_add_number(..)`,
dropping a `TooManyNumbers` error and falling back to the unfolded form. That is
correct (folding must never reject a program that compiles without it), but the
fallback rule lives in two places.
- Fix: one `try_fold_literal(value) -> Option<u16>` that owns the NaN, -0.0 and
  full-pool policy.

### 4.3 Compiler output gets weaker verification than snapshot input
`compiler::verify::validate_bytecode` checks each chunk on its own. The
tree-level checks (each child's `UpvalueDesc::Local(slot)` within the parent's
frame, `Upvalue(i)` within the parent's upvalue count, chunk depth against
`MAX_CHUNK_NESTING`) exist only in `vm/save_state/verify.rs`
`validate_nested_graph`, over `SavedBytecode`. `finalize` never runs them. So a
compiler bug in upvalue resolution (upvalue.rs, see 5.1) ships to the VM
unverified on the compile path and is caught only if the program is later saved
and loaded. The verifier is split between two modules by data type rather than
by concern.
- Fix: move the tree checks into `compiler/verify.rs` behind `BytecodeView` (add
  `upvalue(i)` and `nested(i)` accessors) and call them from both `finalize` and
  `verify_payload`.
- Enforceable: by structure. The examples-corpus test then covers it.

## 5. Policy invented per call site

### 5.1 Upvalue resolution is written twice
`resolve_upvalue_recursive` and `create_parent_upvalue` (upvalue.rs) implement
the same lookup (enclosing locals, then enclosing upvalues, then recurse, then
add) at two different "current" levels. One works on `self.upvalues`, the other
on `self.outer_funcs[parent_idx].upvalues`. The capacity check is also spelled
twice (`add_upvalue`, `checked_parent_upvalue_index`).
- Fix: one recursive `resolve_in(level, name) -> Option<u8>` over a uniform
  stack of `FuncState` that includes the current function (see 7.1). This is
  reference Lua's `singlevaraux`.
- Enforceable: structurally.

### 5.2 Line attribution depends on which token-consumption helper a site used
`current_line` is updated only in `Parser::expect` and at statement start
(`parse_statements_inner`). Every other consumer (`input.next()`, `try_pop`,
`check_type`) leaves it alone. The line stamped on an emitted instruction
therefore depends on whether the nearest earlier token was taken with `expect`
or with `next`. In a multi-line expression such as `local x = a +\n b +\n c`,
the arithmetic ops carry the statement's line. A closing `)` taken by `expect`
on a later line moves everything after it. Calls work around this with an
explicit `CallSite` line. This affects runtime error line numbers, which are
operator-facing but not replay-visible. Not verified against reference Lua line
numbers in this pass; a probe is warranted.
- Fix: update the line in `TokenStream::next`, i.e. the parser always knows the
  line of the last consumed token, and stamp operators with the operator
  token's line, as reference `luaK_fixline` does.
- Enforceable: a line-info golden test over multi-line expressions.

### 5.3 The syntax-depth bracket is hand-rolled at every recursive entry
`enter_syntax_level()?; let r = inner(); exit_syntax_level(); r` appears in
`parse_statements`, `parse_expr`, `parse_unary` and `parse_if_arm`, and doubled
in `parse_prefix_extension`. A future entry point that forgets the `exit`, or
returns early with `?` between the two calls, leaks depth. That bounds
later-legal programs and does not crash, so it would be silent.
- Fix: `fn with_syntax_level<T>(&mut self, cost: u32, f: impl FnOnce(&mut Self)
  -> Result<T>) -> Result<T>`.
- Enforceable: structurally, if `syntax_depth` is touched only inside that
  helper. A grep rule in `brokkr check` can back that up.

### 5.4 Call-site marker emission is duplicated
The `LParen | LiteralString | LCurly` and `Colon` arms of `parse_prefix_suffix`
both emit `mark_call_base(1)`, parse the args, call `open_tail`, NOP the marker
for fixed counts and build a `CallSite`. They differ only in the
`dup/get_field/swap` prelude and the implicit-arg count. The long comments are
copied too.
- Fix: one `finish_call(mark_idx, implicit_args)` helper.

## 6. Ambient dependencies

The front end reads no clock, environment, RNG or working directory in
production code. Lexing is byte- and ASCII-based (`is_ascii_*`,
`is_lua_whitespace`), so it is locale-independent. Decimal parsing uses Rust
core's correctly rounded `str::parse::<f64>`. Hex uses an in-crate exact
algorithm. Both are host-independent. `IndexMap` goes through `det_hash`. No
findings of the ambient kind. The dependency findings here are module-level:

### 6.1 The compiler depends on the VM; runtime cache types live in the compiler
`compiler.rs` imports `vm::{ObjectPtr, Val}` because it defines
`GlobalLookupCacheEntry`, `FieldLookupCacheEntry`, `MethodLookupCacheEntry`,
`StringMethodCacheEntry`, `TforCursorEntry`, their `*Slot` wrappers and
`RuntimeCaches` (with an `unsafe impl Sync`). None of these is compiler output.
`Bytecode`'s own doc comment says the caches live in the State's runtime bundle,
and every consumer is in `vm/` (`eval_store.rs`, `eval_index.rs`,
`eval_control.rs`, `frame.rs`, `vm.rs`). The parser also calls
`crate::vm::check_string_size` (1.10). The compiler should be a leaf that
produces `Bytecode` from `&str` with no knowledge of heap pointers.
- Fix: move every cache type and `RuntimeCaches` to `vm/caches.rs`. The compiler
  then exposes only the slot counts.
- Enforceable: a dependency rule, i.e. a `brokkr check` grep that
  `src/compiler/**` contains no `crate::vm` or `super::vm`. Nothing like it
  exists today.

## 7. Shared state and invariants

### 7.1 Per-function parser state exists twice and is moved field by field
The per-function fields (`chunk`, `literal_indices`, `locals`, `upvalues`,
`loop_breaks`, `fold_barrier`) are declared on `Parser`, declared again on
`FuncState`, listed again in `take_func_state`, listed again in
`restore_func_state`, and initialized again in `parse_str_named`. That is five
restatements. The `FuncState` doc comment records the bug this has already
caused: `loop_breaks` sat outside the unit and a `break` patched the wrong
chunk. Adding a seventh per-function field still means editing five places, and
nothing fails if one is missed (`Default` fills it in silently).
- Fix: `Parser { func: FuncState<'a>, outer_funcs: Vec<FuncState<'a>>, ... }`.
  Nesting becomes `outer_funcs.push(mem::take(&mut self.func))` and restoring
  becomes `mem::replace`. Better still, keep a single `funcs: Vec<FuncState>`
  whose last element is current. That also unifies upvalue resolution (5.1).
- Enforceable: structurally. Once the fields live only on `FuncState`, a
  forgotten save is unrepresentable.

### 7.2 A dummy root frame underwrites the chunk-depth accounting
`parse_str_named` builds a `Parser` whose initial `chunk` exists only to carry
`source`. `parse_chunk` for the root immediately suspends it into
`outer_funcs[0]`. So at top level `outer_funcs.len() == 1`, and that off-by-one
is what makes `outer_funcs.len() >= MAX_CHUNK_NESTING` mean "depth including the
root", as `limits.rs` documents. Root-level upvalue resolution also searches
this empty dummy every time. The correctness of a documented limit rests on an
accidental empty frame.
- Fix: with 7.1's `funcs` stack, depth is `funcs.len()` with no dummy, and the
  source name becomes a `Parser` field instead of riding in a throwaway chunk.

### 7.3 The literal pools are two writers' invariant
`chunk.string_literals`/`number_literals` (the canonical vectors) and
`literal_indices.strings`/`numbers` (lookup maps) must stay in step.
`find_or_add_*` writes both, and the string map stores a second full copy of
every string (`bytes.to_vec()` twice), which doubles pool memory up to the
16 MiB source cap. Any future code that pushes to a pool directly, as
`push_arith` already manipulates `code` directly, desynchronizes them silently.
- Fix: make the pool an `IndexSet<Vec<u8>>` (det_hash), where the index is the
  position, and convert to `Vec` in `finalize`. Numbers need the custom key, so
  keep `IndexMap<u64, f64>` or an `IndexSet` of a key newtype. Either way it is
  one structure and one writer.
- Enforceable: structurally.

### 7.4 `unsafe impl Sync for RuntimeCaches` rests on a non-structural invariant
Strictly VM scope, but the impl lives in compiler.rs. The safety comment argues
"every access path goes through `&mut State`". `RuntimeCaches` sits inside
`Arc<BytecodeRuntime>`, which is cloned into heap closures (`object.rs`) and
frames (`frame.rs`). Soundness actually rests on no `Arc<BytecodeRuntime>` ever
escaping the State, for example through a future host-facing function handle.
That holds today by convention, not by type.
- Fix: make `BytecodeRuntime` `pub(super)` to `vm` with a private constructor
  (already mostly true), and say in the SAFETY comment what must stay true ("no
  `Arc<BytecodeRuntime>` leaves the owning State"). Or drop `Sync` and wrap in a
  `!Sync`-preserving `Send`-only handle (e.g. a `SendCell` with an
  `unsafe impl Send` whose obligation is just "moved with its State").
- Enforceable: partly, by visibility.

## 8. Unbounded growth

Parser-side growth is bounded by `MAX_SOURCE_BYTES`. That covers code length,
the `linebreaks` vector, pools (also capped by `MAX_LITERALS`), the
nested-function count (255 per chunk) and depth (200). Recursion is bounded by
`MAX_SYNTAX_DEPTH`. Pattern-free, so no exponential backtracking. The only items:
- 7.3's doubled string storage, up to about 2x the source size held twice during
  compilation.
- `verify_stack_discipline` clones `AbsState` (with two `Vec<u32>` marker stacks
  of up to 200 entries each) into every reachable pc. That is
  O(code_len x marker depth) memory per chunk, bounded but large for 16 MiB
  sources. Storing marker stacks as persistent or shared structures, or only at
  join points, would bound it by joins.

## 9. Code that is no longer load-bearing

### 9.1 `Clone` and `PartialEq` impls on cache slots
`GlobalLookupCacheSlot`, `FieldLookupCacheSlot` and `SetFieldLookupCacheSlot`
each have hand-written `Clone` ("cloned chunks start cold") and
`PartialEq` (always `true`). They date from when caches lived inside `Bytecode`,
which derives `Clone`/`PartialEq`. Caches now live in `RuntimeCaches`, which
derives only `Debug`/`Default`, and no code clones or compares a slot (grep:
slot types appear only as `&Slot` parameters in `vm/`). `TforCursorSlot`,
added later, has neither impl, which confirms they are unnecessary. The comment
"cloned chunks start cold" is stale. An always-true `PartialEq` is a trap if
someone ever relies on it.
- Evidence of deadness: no `.clone()` of a slot and no `RuntimeCaches: Clone`.
  The compiler would reject a slot clone if the impls were removed, so deletion
  is self-checking.

### 9.2 `TokenType::LiteralHexNumber`
Every consumer treats it identically to `LiteralNumber`: `spelling()` shares an
arm, `parse_expr_base`, `describe_token` and `is_expr_start` match both, and
`numeral::parse_lua_numeral` re-detects `0x` itself. The distinction carries no
information past the lexer, apart from lexer tests.
- Fix: delete the variant. Lexer tests assert `LiteralNumber`.

### 9.3 Thin wrapper layers
- `Parser::describe_token_type` just returns `typ.spelling()`.
- `parse_fndef`, `parse_fndef_named` and `parse_fndef_method` are one-line
  forwards to `parse_fndef_with_params`.
- There are two layers of `parse_str`/`parse_str_named`: `compiler::` wraps
  `parser::`, and each `parse_str` is `parse_str_named(.., None)`. `lib.rs`
  calls `compiler::parse_str` twice and `parse_str_named` once.
- One `compile(source: &str, name: Option<Arc<str>>)` entry would replace four
  functions. `parser::parse_str` exists separately only so tests can see
  pre-`finalize` output, and that can be a `#[cfg(test)]` helper.

### 9.4 Likely-unreachable branches in `numeral::scale_finite_by_power_of_two`
Its only caller is `finish_hex`'s normal path, which runs only when
`top_exponent >= MIN_NORMAL_EXPONENT`. Rounding can only raise the exponent (a
carry), so `exponent < HALF_MIN_SUBNORMAL_EXPONENT` and
`exponent < MIN_NORMAL_EXPONENT` cannot be taken. Subnormals are handled earlier
by `round_hex_subnormal`. The `> MAX_NORMAL_EXPONENT` branch is reachable
through a rounding carry and stays. This is worth confirming with a coverage run
before deleting, since this is numeric code where being wrong is
replay-visible. If confirmed dead, it is an untested second subnormal algorithm
that disagrees in method (double multiply) with the one that is used (integer
rounding).

### 9.5 `parse_decimal`'s `unwrap_or(f64::NAN)`
`let parsed = text.parse(); if parsed.is_err() { return None; }
Some(parsed.unwrap_or(f64::NAN))`. The NaN fallback cannot happen and exists to
dodge `unwrap_used`. Replace with `text.parse().map_err(drop).ok()`. That is
banned (`Result::ok`), so use `match`. Small, but a reader must stop to prove
NaN cannot leak into a literal pool.

### 9.6 `#[hotpath::measure]` on `Parser::update_line`
A one-field setter called per token. When instrumentation is on, it inflates
the per-call event queue and distorts the distribution `--hotpath` reports for
the parser. It is not dead code but a measurement point with no payoff.

## 10. Host assumptions and writes outside owned locations

Production code in scope makes no host assumptions: no paths, no filesystem, no
core count, no locale, no environment. Test code shared with production:
- `compiler.rs` `compiler_examples_pass_stack_discipline_verification` walks
  `env!("CARGO_MANIFEST_DIR")/examples` and requires more than 20 `.lua` files.
  This is a compile-time absolute path, so it works from any working directory.
  The walk follows `read_dir` order and then sorts, so it does not depend on
  filesystem order or case sensitivity. It reads the same tree as
  `tests/run_examples.rs`, so this is a second walker of the examples corpus
  with its own extension filter. A shared `tests/common` corpus lister cannot be
  used from a unit test, so the duplication is forced by the unit/integration
  boundary. What keeps the copies in step is nothing. If examples gain a second
  extension or a skip convention (`-- SKIP-DIFF` style), the walkers diverge.
- `deepest_nested_programs_fit_verifier_marker_limit` spawns an 8 MiB thread
  because debug-build frames are fat. The constant assumes the debug frame size
  on today's toolchain and target. On a target with bigger frames it fails as a
  stack overflow (abort), not as an assertion. This is the same class as the
  documented `syntax_depth_headroom` concern. The stack size could be derived
  from a named constant shared with that test.

No writes anywhere.

---

## Lateral findings (outside the hygiene questions or the scope)

- **`f64::powf` in dispatch is a cross-host replay hazard beyond folding.**
  `vm/frame.rs` `OP_POW` calls `f64::powf`, which is the host libm. The compiler
  carefully refuses to fold `^` so bytecode carries no host result, but runtime
  `^` still gives host-dependent bits, and those feed replays the moment a
  replay crosses hosts. `docs/determinism.md` presumably scopes determinism to
  one host and build. If cross-host replay is ever wanted, the fix is an
  in-crate correctly rounded `pow` owned by `numeral.rs`, plus a clippy
  `disallowed_methods` entry for `f64::powf`, `powi`, `sin`, `exp` and the rest
  in library code (with `math.rs` routed through the same owner). The same
  question applies to `numeral.rs`'s own `2_f64.powi`, which is exact for powers
  of two but is still an LLVM intrinsic.
- **`MAX_SYNTAX_DEPTH`'s effective meaning differs by construct** (2.1). Nested
  calls get 100 levels, nested parentheses 200. Reference Lua's `LUAI_MAXCCALLS`
  is also per-construct, so this may be acceptable, but it is undocumented.
- **`finalize` recursion runs `validate_bytecode` once per chunk and then
  recurses**, so a 16 MiB source with 255 x 200 nested chunks pays for the
  verifier's per-pc state clones (section 8) many times. That is bounded, but
  it is the biggest compile-time cost multiplier in the front end, and it runs
  in release on every `load`.
- **`table.rs` comment cites an example file and DIFF tag** for the
  leading-separator divergence. Good practice, but `docs/compatibility.md` is
  the durable owner of divergences, and the comment should point there.
- **`exp_desc.rs` comment cites "the capacity findings"**, which is
  transient-document language, from a durable code comment. The comment should
  carry the reasoning itself, which it mostly does. Drop the reference.
- **`parse_pow` comment says "part of the cost model since version 4"**, a
  hard-coded historical version in a code comment. Per the documentation rule,
  reword it to drop the drifting specific, or point at `COST_MODEL_VERSION`'s
  doc history.
