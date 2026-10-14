# hygiene.md

Hygiene findings from the hunt over the compiler, vm, surface, stdlib and
tooling scopes: values with more than one owner, rules implemented more than
once, and assumptions about the host. Hunter reports, not verified findings:
each may be wrong. Defects found along the way are in [bugs.md](bugs.md).

1. An entry is removed entirely when completely resolved. No historical record
   stays here.
2. Stable IDs never change and are never reused; removal leaves a gap.
3. An entry adjudicated against, verified incorrect, or whose outcome is that no
   action is taken owes comments at the code sites it names - and, where the
   claim touches a documented contract, the relevant `reference/` or `docs/`
   page - before the entry is removed, so the finding is not hunted again.
4. Once all findings are resolved, the file gets deleted.
5. Finding IDs are never written into the code or other documents. They are
   stable only until this document is drained; the next hunt writes new ones,
   and they are never deduplicated through git history. Carry the context
   inline instead.

# Opcode and encoding ownership

## HYG-001 - The opcode set has no single owner, and static and runtime cost are two copies of one table

Reported by the compiler and surface hunters.

`src/instr.rs` defines each opcode as a bare `pub(crate) const OP_X: u8`. Every
other fact about an opcode is a hand-kept full list elsewhere:

| Fact | Where it is spelled |
|---|---|
| number | `instr.rs` consts |
| known opcode | `verify.rs` `known_opcode` (a 60-arm `matches!`) |
| operand bytes that must be zero | `verify.rs` `reserved_zero`, three hand-kept groups plus a nested "A is reserved" list |
| operand-stack effect | `verify.rs` `verify_stack_discipline` |
| control transfer and target | `verify.rs` successor match, the jump-target range check (a second list in the same file), `compiler.rs` `transfer_with_offset`, VM dispatch in `vm/frame.rs` |
| display name | `instr.rs` `impl Debug` |
| cost class | `lib.rs` `ScopeCost::analyze_chunk` (static) and the `add_cost!` sites in `vm/frame.rs` (runtime) |
| constructor shape | the `Instr::xxx()` helpers |

Drift already present:

- `Debug` prints `Concat` with no count and `MarkCallBase` with no adjustment,
  so a `debug_parser` dump loses information the verifier depends on.
- The strip-remap test fixture (`strip_remaps_every_transfer_forward_and_backward`
  in compiler.rs) lists 7 transfer opcodes; `transfer_with_offset` handles 13.
  The six fused `BRANCH_FALSE_*` opcodes (64-69) are covered only indirectly
  through the example corpus.
- The dispatch range `OP_BRANCH_FALSE_LESS..=OP_BRANCH_FALSE_NOT_EQUAL` in
  `vm/frame.rs` works only because 64-69 are contiguous. The fallback arm in
  `eval_control.rs` `instr_branch_fused` turns a violation into a runtime
  `InternalError`, not a build failure.
- The static cost match in `analyze_chunk` (arith, negate, four
  `OP_NEW_TABLE*`, six table-write forms, `OP_SET_LIST`) restates the runtime
  `add_cost!` sites. Only `stripping_preserves_costs...` and a few lib.rs cost
  tests keep them in step. A new costed opcode would charge at runtime and be
  silently free in `analyze_cost`.
- Prose copies have drifted: the `Frame::eval` doc comment still lists "string
  operations" (and length) as free, which predates cost model 2's native
  charging. README "Budget" is a fourth copy.
- Lateral (surface): `CostAnalysis::totals` re-sums `own_cost` across the tree
  while `ScopeCost.total_cost` is computed separately in `analyze_chunk`, a
  third copy of the same sum. `ScopeTotals` mirrors `ScopeCost` field for field
  only to sum them; `ScopeCost` could implement `AddAssign`.

Proposed fix (compiler): one declarative table, a `macro_rules!` taking `(NAME,
number, operand shape, stack effect, transfer kind, cost class, debug
format)`, emitting the consts, `known_opcode`, the reserved-byte mask,
`is_transfer`/`with_offset`, `Debug` and the static cost class. The verifier's
stack-effect match can stay hand-written for dynamic opcodes. Proposed fix
(surface): a `const fn static_cost(opcode) -> Option<CostClass>` that dispatch
charges and the analyzer sums, ideally an exhaustive `match` over an opcode
enum. Enforceable: an exhaustive generated table makes a missing entry a
compile error; add a `const _: () = assert!(...)` that 64..=69 is exactly the
fused group; drive the strip fixture from `is_transfer` over all 256 byte
values; a test compiling a one-instruction chunk per opcode and comparing
analyzer and runtime cost.

## HYG-002 - `Fixed(255)` silently encodes to the `All`/`Dynamic` sentinel

Reported by the compiler hunter; the vm and stdlib hunters raise the same
aliasing at the runtime edge.

`to_u8` maps `Fixed(n)` to `n`, so `RetCount::Fixed(255)` and
`ArgCount::Fixed(255)` encode to the sentinel byte. Compiler sites depend on
this on purpose or by accident:

- `parse_explist` allows 255 expressions and `parse_return` then emits
  `Instr::ret(RetCount::Fixed(255))`, so `return e1, ..., e255` compiles to
  "return all". It gives the same answer only because the operand stack holds
  exactly those values at a statement boundary. Fixed returns and the runtime
  `RetCount::All` path ("too many results (limit 255)" in vm/eval.rs) are not
  the same rule.
- `expr.rs` (call and method-call tails), `stmt.rs` `parse_return` and
  `adjust_multi_assign` rebuild an inner call as `ArgCount::Fixed(call.num_args())`
  / `Fixed(old.a())`; a dynamic inner call stays dynamic only because
  `Fixed(255)` aliases `Dynamic`. `table.rs` uses `ArgCount::from_u8(instr.a())`,
  the correct spelling of the same operation.
- `CallSite` stores `num_args: u8` with 255 meaning dynamic and no type saying
  so.
- vm: the `__call` path computes `combined = actual + 1`, which can reach 255;
  harmless as an enum today but worth a test.
- stdlib: `unpack` deliberately allows 255 results (`span >= 255` rejects
  256+); whether a Rust function returning exactly 255 values collides with the
  `All` sentinel is worth checking.

Fix: `Fixed` holds a newtype whose constructor rejects 255, `CallSite` stores
an `ArgCount`, and `parse_explist`/`parse_return` check against the fixed-count
limit. Enforceable by type; also a test that `return` of 255 literals produces
`ret(Fixed)` or a syntax error.

## HYG-003 - "Uncached" has two encodings

Reported by the compiler hunter; narrowed by a fixer, who documented the
encoding contract at the instr.rs, `assign_cache_slots` and verifier sites.

`GET_GLOBAL` and `GET_FIELD`/`SET_FIELD` use 255 for "uncached"; `SET_GLOBAL`
and `TFOR_CALL` use a +1 bias with 0 for "uncached". Snapshots store raw
instruction words, so unifying them behind a `CacheSlot` newtype needs a save
format bump plus coordinated changes in compiler, verifier, save_state and the
runtime decoders (`eval_index.rs`, `eval_store.rs`). Worth doing alongside the
next format bump.

## HYG-006 - Opcode numbers and builtin indices are part of the save format, but nothing ties them to `FORMAT_VERSION`

Reported by the compiler hunter.

Snapshots store raw `u32` instructions and builtin slots by index.
Renumbering either changes what an existing snapshot means without moving
`FORMAT_VERSION`. The fused-branch CHANGELOG entry relies on "unknown opcode"
rejection, which covers additions only. Enforceable: a golden test pinning the
opcode-number and builtin-index tables next to `FORMAT_VERSION`.

## HYG-007 - Cache-slot allocation and the tfor-cursor count are each derived twice

Reported by the compiler hunter. Both copies are deliberate; recorded with
what keeps them in step.

- `assign_cache_slots` counts `tfor_cursor_len` and discards it;
  `RuntimeCaches::new` recomputes it as `max(C)` over the code, because a
  fourth count field would change the snapshot format. The verifier's
  sequential slot check keeps them in step. The comment hard-codes "format 6";
  reword to "the snapshot format".
- The verifier restates the cache-slot allocation algorithm exactly
  (first-use order for globals, instruction order for fields and set-fields),
  held by `compiler_examples_pass_stack_discipline_verification`. Any policy
  change (for example sharing GET_FIELD slots by name) must change both.

# Limits and tunables

## HYG-008 - Compiler limits are bare 255/254 literals, restated in error text, and one has already diverged

Reported by the compiler and surface hunters.

- Parser: `add_local` (`u8::MAX`), `checked_fixed_arg_count` (`254`),
  `add_upvalue`/`checked_parent_upvalue_index` (`u8::MAX`), the nested-function
  cap in `parse_fndef_named` and `parse_fndef_method` (once each),
  `try_use_table_template` (`u8::MAX`, twice), table batches (`i == u8::MAX`).
- Verifier: `validate_bytecode` restates them (`len > 255`, `>
  usize::from(u16::MAX) + 1`, `num_params + num_locals > 255`).
- error.rs Display: `"(limit 255)"`, `"(limit 254)"`, independent of the
  checks.
- README: "255 locals and 255 upvalues, and 65536 ...".

Diverged: `TooManyExpressions` says "limit 255", but
`checked_multi_assign_width` raises it at width exactly 255, so the real limit
there is 254. The same variant also covers concat chains over 255 and the
`set_field_at`/`set_table` offset overflow - one message for three limits.
`TooManyNestedFunctions`' doc says "nests function definitions deeper than the
VM accepts", but the check (`parser/func.rs`) is on the count of direct child
functions in one chunk (`nested.len() >= u8::MAX`); depth is governed by
`TooManySyntaxLevels`.

Fix: a `limits` module (`MAX_LOCALS`, `MAX_UPVALUES`, `MAX_FIXED_ARGS`,
`MAX_NESTED`, `MAX_LITERALS`, ...) that parser, verifier and Display
interpolate. Enforceable: one test per constant compiling N and N+1 and
checking the message names N.

## HYG-009 - `MAX_SYNTAX_DEPTH` carries three meanings

Reported by the compiler and surface hunters.

It bounds parser recursion (parser.rs), the verifier's call-base and
table-marker stacks (verify.rs, "marker stack is too deep"), and snapshot
nested-chunk depth (`vm/save_state/verify.rs` `validate_nested_graph`). The
second and third hold only because the parser spends at least one syntax level
per marker and per nested function. Each paren spends two (the parser test
notes an effective paren limit of about 99), so the README's "bounds syntax
nesting at 200 levels" overstates it for parentheses. If the parser ever
charges fewer levels, the verifier rejects the compiler's own output. The
surface hunter calls the snapshot reuse a good single owner but a borrowed
meaning, and proposes a dedicated `MAX_CHUNK_NESTING` the parser enforces
directly. Fix: give each meaning its own named constant, derived with a
comment. Enforceable: a test compiling the deepest accepted program and
checking the marker depth it reaches against the verifier constant. Reword
the README to describe the budget rather than a depth.

## HYG-010 - Runtime limits are private constants, restated as numbers in prose, with no injection point and no list

Reported by the vm, surface and stdlib hunters.

`MAX_CALL_DEPTH = 1000` and `MAX_STACK_SIZE = 1_000_000` (vm.rs, private),
`MAX_METAMETHOD_DEPTH = 200` (vm/metamethod.rs, private), `MAX_SYNTAX_DEPTH`
(`pub(crate)`), `GMATCH_PATTERN_CACHE_ENTRIES`/`_BYTES` (vm.rs),
`COST_CHECK_INTERVAL` (frame.rs), and the parser's 254/255 literals. Only
`MAX_STRING_BYTES` is public. README restates "200 levels", "1000 frames",
"1,000,000 values" and "16 MiB" (twice); AGENTS.md restates 1000 and
1_000_000; error.rs doc comments say "runs 1000 frames deep"; an eval.rs
comment says "MAX_CALL_DEPTH = 1000". The error variants
`CallDepthExceeded`, `StackOverflow` and `MetamethodDepthExceeded` carry no
`limit` field, unlike `StringSizeExceeded { size, limit }`.

No injection point: `call_depth_exceeded_error` has to recurse a real 1000
frames on a big-stack thread, the stack-cap tests fill a million slots, and
public types in error.rs (`PrefixLocation::TraceFrame`, `ScriptError`) are
shaped around debug-build stack size at depth 1000.

Fix: a `pub mod limits` (or `State::limits()`) re-exporting every limit; give
every limit error a `limit` field; reword prose to cite the name. A per-State
`Limits` struct (defaulted, fixed at construction for determinism, recorded by
save/restore) would let tests use small caps and the game tune per script.
Enforceable: a test that every `const MAX_*` in `src/` is re-exported; a
doctest asserting README numbers equal the consts.

## HYG-011 - The 255-argument/result runtime limit has several ad hoc spellings and texts

Reported by the vm and stdlib hunters.

`"too many arguments (limit 255)"` twice and `"too many results (limit 255)"`
once in vm/eval.rs; `select` (basic.rs) uses `u8::try_from(count)` with the
same "too many results (limit 255)" text; `unpack_values` (lua_std.rs) uses
`span >= 255` with "too many results to unpack" (twice in one function). These
are `RuntimeError` strings, while the compiler side has typed variants. The
real owner is the `u8` return of `RustFunc` and `RetCount`'s width. Fix:
`ErrorKind::TooManyArguments / TooManyResults { limit }` derived from
`u8::MAX`, a `MAX_RESULTS` const and `results_count(n) -> Result<u8>` helper
next to `RetCount`; unit test at 255/256. A brokkr text rule could then reject
`"limit 255"` literals.

## HYG-012 - GC policy and the auto-GC sentinel have two owners

Reported by the vm hunter.

`State::GC_INITIAL_THRESHOLD = 20` (vm.rs) and a literal `.max(20)` in
`GcHeap::collect` (vm/object.rs), with the growth factor `2` inline there;
tests restate `>= 20` and `gc_set_threshold(20)`. `usize::MAX` as "auto-GC
disabled" is spelled in `gc_disable_auto`, in `GcHeap::collect`, and in doc
comments. Fix: initial, floor and growth as named consts or a `GcPolicy` in
`GcHeap`; `Option<usize>` or an enum threshold so the sentinel is
unrepresentable.

## HYG-013 - Capacity hints and headroom are hand-counted, and some are already off

Reported by the vm and stdlib hunters.

`stack` 256, `call_stack` 64, `transient_roots` 64, table `INLINE_CAPACITY 4`,
`reserve_stdlib_capacity` (`globals.reserve(Builtin::COUNT + 4)`,
`heap.reserve(8, 96)` - VM magic numbers describing the library's shape),
`new_table_with_capacity(24)` for math (22 entries registered), `(10)` for
string (10), `(8)` for table (7). `STDLIB_SETUP_HEADROOM = 8` (lua_std.rs) is
justified in prose ("four slots") and checked by nothing. Nothing lists the
tunables. Fix: per-library `const FUNCTIONS: &[(&str, RustFunc)]` with
capacities from `.len()`; named consts in one tuning section; a test that the
stack high-water mark during `open_libs` stays within the headroom.

## HYG-014 - The table-library fast path silently disables itself if the function count changes

Reported by the stdlib hunter.

`TableLibraryFallbackCache.names: [Val; 7]` (vm.rs), and
`capture_table_library_fallback` does `live_string_keys().try_into()`; any
other count sets the cache to `None` with no failure and no log. The count's
owner is `open_table`. Fix: derive the length from table.rs's function list
(`[Val; TABLE_FUNCTIONS.len()]`), making the coupling a compile error; test
that the fallback is `Some` after `open_libs`.

## HYG-015 - `COST_CHECK_INTERVAL` may or may not be observable, and nothing says which

Reported by the vm and surface hunters.

`COST_CHECK_INTERVAL = 64` (vm/frame.rs) has no injection point; its doc says
higher values "may overshoot budget more". The `add_cost!` macro also flushes
when `remaining - next <= 0`, so the vm hunter reads it as not observable,
while the surface hunter notes nothing documents whether it can move the point
`BudgetExceeded` fires (and so `cost_used` at failure). If it can, it is part
of the determinism contract and belongs with `COST_MODEL_VERSION`; if it
cannot, a property test should hold that, like the `consume_units` test in
cost_meter.rs.

## HYG-016 - Line numbers have two widths

Reported by the compiler hunter.

`CallSite` saturates call lines at 2^24, while `line_info` is `u32`. In a
source longer than 16M lines, a call's traceback line freezes while
neighbouring instructions report the true line. Use one width or one
saturation rule.

# Cost model

## HYG-017 - `COST_MODEL_VERSION` did not move when compiler emission changed `cost_used`

Reported by the compiler and surface hunters.

The CHANGELOG Unreleased constant-folding entry says folded literal arithmetic
no longer charges, so `cost_used` decreases for identical source, and asks
users to re-baseline fingerprints. `COST_MODEL_VERSION` is still 2 and its doc
describes only v1 and v2; CHANGELOG 0.4.0 says it exists "so hosts can identify
the accounting model independently of the crate version". A save written
before the change and loaded after carries old-model `cost_used`/`cost_remaining`
and the version gate accepts it. Needs an author decision: either the version
covers "cost of a given source" (bump it, and it now moves with compiler
emission), or only per-opcode charges (reword doc and CHANGELOG so hosts do not
use it as a fingerprint key). Enforceable: a golden test pinning `cost_used`
for a fixed corpus with an assertion message naming `COST_MODEL_VERSION`, the
pattern `save_golden.rs` uses for the format.

## HYG-018 - Per-function native cost rules are scattered literals, inconsistent with each other and with the README

Reported by the stdlib hunter.

README "Budget" says data-dependent native work in string, pattern and table
libraries is charged. The rules are per-closure literals:

- `find`, `match`, `gsub` charge `(s + p).max(1)` and then `p.max(1)` again
  (pattern bytes twice); `gmatch` charges `(s + p)` at construction and
  `p.max(1)` at first iteration via `memoize_gmatch_pattern` (vm.rs) - a fourth
  rule, owned by the VM.
- `string.len` free; `sub` charges output bytes but not a number-to-string
  coercion; `upper`/`lower`/`reverse` charge input bytes.
- `table.insert`/`remove` charge a flat 1 regardless of shift (unless
  `table_insert_at` charges internally, splitting the rule across library and
  VM); `table.pack` charges 1 for any argument count; `unpack` (both spellings)
  charges nothing for up to 255 `get_table` calls, each of which may invoke
  `__index`.
- `print` builds up to a 16 MiB message free; `tonumber` parses up to a 16 MiB
  string free (including `parse_integer_with_base`, an unbounded f64
  accumulation); `tostring`, `select`, `next`, `ipairs` free.
- math.rs says "costs 1" on every function: 20 copies of one constant.

Nothing answers "what does each stdlib function cost", and `analyze_cost`
cannot see any of it. Fix: a declarative registration table per library
(`&[LibFn { name, func, cost: CostRule }]` with `Flat(1)`, `PerByte`,
`PerElement`) and a doc-tested reference table generated from it. Enforceable:
a test that every registration has a cost rule; a cost-snapshot test per
function (fixed inputs, exact `cost_used`).

## HYG-019 - Compile work is uncharged and unbounded

Reported by the compiler hunter.

`find_or_add_string_bytes` and `find_or_add` are linear scans, so building the
literal pools is quadratic - up to 65536 x 65536 / 2 byte-slice compares per
function. `analyze_cost` and `load_string` charge nothing for parsing and the
source length has no cap, so a host loading untrusted scripts can be stalled at
load time. Fix: an `IndexMap` keyed by bytes for the pools, and a documented,
configurable maximum source size.

# Errors and diagnostics

## HYG-020 - "bad argument" has five owners and two formats, and type checks name no function

Reported by the stdlib hunter; the vm hunter reports the hand-built `ArgError`
sites.

`State::check_type` (vm_aux.rs) builds `ArgError { func_name: None, .. }`, and
`ArgError`'s Display (error.rs) renders `None` as `<anonymous>` and `Some(name)`
unquoted. Nearly every library type check goes through it, so `math.sin("x")`,
`string.sub("a", {})` and `table.insert(1, 2)` say `to <anonymous>`; reference
says `to 'sin'`. Hand-written messages in the same library quote:
`exact_integer_argument` (lua_std.rs), `position_out_of_bounds` (table.rs),
`format_arg_error` (string_format.rs, which also re-spells "X expected, got Y"),
and literals in math.rs (3), basic.rs `error` and string.rs `gsub`. So one
function can say `to 'insert'` or `to <anonymous>` depending on which argument
was wrong; no test pins `<anonymous>`. `ArgError` is also hand-built with
`func_name: None` in vm.rs (2), vm/stack.rs (2) and string.rs
`string_or_number_bytes`. Arg-count policy is per function too: `table.insert`
and `math.random` each spell `"wrong number of arguments to '<name>'"`, and
`math.min`/`max` hand-build a zero-arg `ArgError` that `check_type(1, Number)`
would produce identically. The `unpack` wrappers mean `unpack` and
`table.unpack` both say `'unpack'`.

Fix: record the executing Rust function's registered name in its CallInfo
(snapshot builds already carry `"math.sin"`), have `check_type` fill
`func_name`, quote it in Display, and give `ArgError` a `detail: ArgDetail`
enum (TypeMismatch, NoInteger, OutOfBounds, IntervalEmpty, Custom) with one
renderer. Enforceable: a text rule forbidding the literal `"bad argument`
outside error.rs; a table-driven test calling every registered stdlib function
with a wrong-typed argument and asserting the message contains `'<name>'`.

## HYG-021 - Cast lints are not yet enforced in the stdlib

Reported by the stdlib hunter; narrowed by a fixer, who routed 21 integer
argument sites through `arg_integer`/`opt_integer` in lua_std.rs.

Remaining: `clippy::cast_possible_truncation` and `cast_sign_loss` at module
level for `src/lua_std/` would keep the next argument from bypassing the
helper. The remaining bounded index and result `as` conversions need an audit
first.

## HYG-022 - Error kinds are mislabelled, and "no location" has two spellings

Reported by the vm and compiler hunters.

- `InvalidStackIndex` renders as `"internal error: invalid stack index"`
  (error.rs) but is the documented result of host misuse of public
  `set_top`/`pop`/`convert_idx`.
- `push_named_rust_fn` (stack.rs) and table_ops.rs map
  `SaveError::DuplicateFunctionRegistration` (an embedder bug) to
  `InternalError` ("VM bug"), stringifying it and losing the id.
- gmatch maps `PatternError` to `RuntimeError(String)`, shedding structure.
- vm: `self.error(kind)` and `Error::without_location(kind)` are used
  interchangeably; `State::error` takes `&self` and ignores it.
- compiler: internal errors are built three ways - `compiler.rs`
  `internal_error` (no location), `parser.rs` `parse_chunk`
  (`error_at(InternalError, 0)`, which renders a false line 1 col 1), and
  `finalize` (inline `without_location`).

Fix: one helper per kind; `State::error` removed from the public API or made to
locate.

## HYG-023 - Compiler errors lose their subject

Reported by the compiler hunter.

- `finalize` reports `"compiler emitted invalid bytecode: {reason}"` and drops
  `BytecodeValidationError.instruction` (the pc), the chunk `name` and its
  `source`, so a field report names no function and no instruction. The
  `#[cfg_attr(not(feature = "snapshot"), allow(dead_code))]` on `reason` is
  stale, since `finalize` reads it in every build.
- Syntax errors carry line and column only; `parse_str_named` has the source
  name but never attaches it.
- Operator text assembled in the parser instead of error.rs Display:
  `UnexpectedTok(format!("'{exp}' expected near {got}"))` and
  `UnexpectedTok("cannot use '...' outside a vararg function")`, which should be
  its own `SyntaxError` variant.
- Keyword spellings live in `lexer.rs` `keyword_match` and `parser.rs`
  `describe_token_type`; one `TokenType -> &'static str` table could drive both.

## HYG-024 - Aborts on caller-controlled input

Reported by the compiler and vm hunters; BUG-011 is the `get_table` case.

- Compiler, for sources over 4 GiB (nothing caps source size): `Token.len` is
  `u32` from `(self.pos - tok_start) as u32` and `tok_line` from
  `self.linebreaks.len() as u32`; both wrap silently, and a wrapped `len` feeds
  `substring` (a slice that can land off a char boundary and panic) and
  `assert!(len >= 2)` in `get_literal_string_contents`. Cap source length at the
  entry point with a typed error, or use `try_from`. The `peek_equals` `panic!`
  arms and `unreachable!`s are internal-only.
- Public API (vm): `check_any`, `check_type`, `is_none_or_nil`
  `assert!(arg_number != 0)` (vm_aux.rs); `Val`'s `Hash` impl hard-asserts
  non-NaN (lua_val.rs), safe only while every insertion path pre-rejects NaN;
  `insert`/`remove`/`replace` accept whatever `convert_idx` accepts and are
  otherwise unvalidated.
- Enforcement idea (vm): `clippy::panic` / `clippy::manual_assert` scoped to the
  public API.

## HYG-025 - The `on_error` once-only rule is an implicit sentinel

Reported by the vm hunter; narrowed by a fixer, who centralised frame-exit
cleanup in eval.rs.

`call()` fires `on_error` at depth 0 with an empty trace and
`eval_closure_frame` fires it when it attaches a trace; "empty stack_trace
means not yet reported" is implicit, now documented by comments at both sites.
A `reported: bool` on `Error` (error.rs) would be structural.

## HYG-026 - Snapshot errors merge bugs with refusals and shed what an operator needs

Reported by the surface hunter.

- `SaveError::EncodeError(String)` is returned from about 20 sites in
  save_state.rs with fixed strings, mixing invariant violations ("unfilled
  object slot", "missing root slot", "missing table entry slot", "missing
  environment baseline", "environment object is not a table") with resource
  refusals ("too many strings/objects/upvalues/chunks", "vector too long"). Split
  into `SaveError::Internal(&'static str)` and `SaveError::TooLarge { what,
  count }`.
- `LoadError::UnsupportedVersion` and `UnsupportedCostModelVersion` carry
  neither found nor expected values; add `{ found, expected }`.
- `CorruptArena` comes from about 20 distinct checks in verify.rs and from
  `object_ref`/`upvalues_ref`, all rendering "corrupt save arena". Give it a
  `reason: &'static str` and an index, like `InvalidBytecode`.
- `DecodeError(String)` messages carry no byte offset though `Decoder.pos` is
  there.
- verify.rs `invalid()` maps an overflowing chunk index to `u32::MAX`
  (`unwrap_or(u32::MAX)`), a fabricated id; the index is bounded by a `u32`
  length prefix, so the fallback is dead and should be an `expect` naming that
  bound.
- `DuplicateFunctionRegistration` is a `SaveError` raised by
  `State::register_rust_fn` at registration time, which is why
  `set_global_stdlib_rust_fn` must `.expect(...)` it; a `RegistrationError` or
  `error::Error` variant would match when it happens.
- `on_error` is not fired for save or load failures and there is no logging on
  that path, so the "single source of telemetry" claim in `on_error`'s doc does
  not cover persistence.

## HYG-027 - `load_state`'s setup closure cannot fail

Reported by the surface hunter.

The signature is `setup: impl FnOnce(&mut State)`, but what a host does in
setup is fallible (`register_rust_fn`, `set_global_named_rust_fn`, `push_*`,
`new_table`, `get_global`, `open_libs`). The host must `expect` (abort during a
load of user-controlled input) or swallow, and a duplicate-registration bug then
surfaces as `LoadError::UnknownFunction` naming the wrong cause. Make it
`FnOnce(&mut State) -> Result<(), E>` with a `LoadError::Setup(E)`.

## HYG-028 - Registration failures are handled three ways in the stdlib

Reported by the stdlib hunter.

The three `add_fn!` macros `.expect("... registration cannot fail")`, basic.rs
propagates `?`, and string.rs maps the snapshot `register_rust_fn` failure to
`InternalError`. `open_libs` is public, and the string.rs mapping shows the
snapshot registry can fail; if `set_table_str_key_named_rust_fn` can fail the
same way, the `expect` is a panic on a host-controlled call. The
`#[cfg(feature = "snapshot")]` named-vs-plain fork is written out six times
(basic.rs `add` closure, basic.rs `_G` metatable twice, the math/string/table
`add_fn!` macros differing only in prefix, string.rs
`push_named_or_plain_rust_fn`). Fix: one `State::register_lib_fn(table_idx,
lib_prefix, name, func)` doing the cfg internally and returning `Result`, with
the underlying methods `pub(super)` to vm. Enforceable by visibility.

## HYG-029 - gmatch iterator state is read with `unwrap_or(0.0)`

Reported by the stdlib hunter.

`gmatch_empty_iter` (twice) and `gmatch_iter` read `pos`/`len` with
`state.to_number(-1).unwrap_or(0.0)`, and `gmatch_empty_iter` does not
`check_type(1, Table)`. Scripts cannot reach the state table, but README says
save files are user-editable input, so a doctored save with a non-number `pos`
silently restarts iteration at 0. Validate in `verify_payload` or make the
iterator error. Enforceable: add `Result::unwrap_or` /
`Result::unwrap_or_default` to `clippy.toml` `disallowed-methods`, allowed at
genuine sites.

## HYG-030 - Save-file strings reach operator logs verbatim and unbounded

Reported by the surface hunter.

`LoadError::UnknownFunction(id)` and `UnknownEnvObject(token)` echo strings
from the save file into Display; README says saves are user-editable, so a
forged save can put terminal escapes or megabytes into a log line. Truncate and
escape in Display. `SaveError::UnregisteredFunction { reachable_from }` renders
a path of global names and table keys (user content), unescaped and unbounded in
length for deep graphs (`SaveBuilder.breadcrumbs`, each `Global(String)`
cloned).

## HYG-031 - The decoder's length policy is per field

Reported by the surface hunter.

`read_lua_string_bytes` caps at `MAX_STRING_BYTES`; `read_bytes` (crate-version
string, chunk names, sources, env tokens, `Fn` ids) has no cap beyond the
remaining bytes; `read_vec` caps reservations at the remaining bytes. `source`
is later cloned into every `StackFrame` on every error. One
`read_bounded(kind, max)` with a per-field cap table would hold the policy.
Duplicate detection is likewise open-coded with fresh `BTreeSet`s in
`validate_environment_deltas` (three) and `validate_pointer_ids` (two); an
`insert_unique(set, key, what)` helper would also give `CorruptArena` its reason
string (HYG-026). Version gating is exact equality only, described per release
in README/CHANGELOG prose; state the policy once next to the constants ("no
migration; exact match; bump on any change an old reader would misread").

## HYG-032 - Table-key equality is re-derived on the load path

Reported by the surface hunter.

verify.rs `resolve_key` normalises `-0.0` to `0` and rejects NaN and nil to
detect duplicate saved keys; save_state.rs `runtime_table_key` does the same job
at materialisation; the VM's table code has the canonical rule. If VM key
normalisation changes, the verifier accepts saves the VM sees as colliding, or
the reverse. Have `resolve_key` call the VM's canonicaliser.

# State, ordering and ambient dependencies

## HYG-033 - `current_source` is set by load order, not by the running frame

Reported by the surface and compiler hunters.

`State::load` writes `current_source`, which callbacks receive as `source` in
`on_print`/`on_error`. Load A, load B, then call A's closure, and A's prints
are attributed to B. `load_string_named` sets it before parsing, so a rejected
load leaves it pointing at the failed chunk. The running chunk's
`Bytecode.source` is on the `CallInfo`/frame; derive it from there and delete
the field.

## HYG-034 - Several invariants rest on call ordering in one function

Reported by the vm, surface and stdlib hunters.

- The gmatch cache `gmatch_patterns: IndexMap<StringPtr, _>` (vm.rs) is safe
  only because `gc_collect`, the sole path that frees strings, clears it first. A
  second collection entry calling `heap.collect()` directly (object.rs tests do)
  would leave keys aliasing a new string at a reused slot, running a different
  pattern. "memoize must precede match" in
  `gmatch_subject_matcher_and_cost_meter` is the same shape. Fix: key by content
  or move the clear into sweep; a typestate token from memoize to match.
- `with_restricted_env` swaps `globals`/`builtins` and bumps `globals_version`
  but does not refresh `table_library_fallback`; safe because the cache compares
  the table ObjectPtr, an argument that lives in the cache's doc comment.
- `State::open_libs` calls `lua_std::open_libs` then
  `capture_table_library_fallback`; other construction paths
  (`with_callbacks`) must remember the pair. Make it one function.
- `pending_bytecode_caches` bridges a GC that may run between
  `resolve_bytecode_runtime` and `alloc_lua_fn` in `push_closure`; correctness
  rests on push/pop order in that function.
- `load_state` clears `registry`, runs `setup`, then `materialize_payload`,
  whose allocators must "never run GC" (module doc) with no type or assertion. A
  `debug_assert` on a GC counter or a `NoGc` token would make it structural.
- `env_tokens` (captured at construction) and `builtins` are two writers of
  "which object is `math`"; if a host calls public `open_libs` twice they
  diverge. Check who refreshes `env_tokens`.
- `validate_quiescent` lists ten fields by hand; a new in-flight vector would
  not be added. Put "is quiescent" on `State` next to the fields and
  `debug_assert!` it at `call`/`eval` entry and exit.

## HYG-035 - GC triggering and temporary rooting are policy per call site

Reported by the vm hunter.

`if self.heap.is_full() { self.gc_collect() }` is copied in `alloc_string`
(vm.rs), `alloc_table`, `alloc_table_with_capacity` (table_ops.rs) and
`push_closure` (eval.rs); one `maybe_collect()` would own it, and allocators
could require a token proving the check ran. Rooting a temporary across
allocation has at least four idioms: push/pop on the value stack (metamethod.rs
`__index`/`__newindex`), `with_rooted_value` (`__call`), manual
`transient_roots` push/pop (the Lua-call path, `resolve_bytecode_runtime`), and
watermark plus `catch_unwind` (`eval_closure_inner` varargs).
`bytes_with_tostring_meta` interns `"__tostring"` without rooting `val` (safe
only because `val` is still on the stack). Metamethod names `"__index"`,
`"__newindex"`, `"__call"`, `"__len"`, `"__tostring"` are interned at each use
site on every dispatch (and `"__index"`/`"__newindex"`/`"__metatable"` again in
basic.rs), with four copies of `.expect("fixed metamethod name is far below
MAX_STRING_BYTES")`. Pre-interning a rooted `MetaName` set per State, like
`Builtin`, removes most of these sites.

## HYG-036 - Panic safety via `catch_unwind` is half a policy

Reported by the vm hunter.

`catch_unwind` is applied in `with_restricted_env`, `with_rooted_values` and
vararg frames, but explicitly not on the main Lua call path (an eval.rs comment
says the State must be discarded after an unwind). A panic anywhere under a Lua
call already leaves the State inconsistent, so the partial guarantees buy
nothing. Either poison the State on unwind (a flag checked on entry) and drop
the per-site guards, or make it uniform.

## HYG-037 - Constant folding of `^` writes the host's libm result into bytecode

Reported by the compiler hunter.

`parse_pow` folds with `f64::powf`, which calls the platform math library and
is not guaranteed bit-identical across targets or libm versions. The VM uses the
same function at runtime, so one host agrees with itself, but snapshots carry
compiled bytecode, so a replay on another host mixes host A's folded constant
with host B's runtime `powf`. `pow` needs one owned implementation (for example a
pure-Rust libm port) used by both, or `^` should not be folded; `+ - * / %` are
IEEE exact. Related fold-ownership point: each fold operator is paired with its
function in the parser (`parse_addition`, `parse_multiplication`, `parse_pow`)
and again in `vm/frame.rs`, guarded only by a comment; an
`arith_fn(opcode) -> fn(f64, f64) -> f64` in numeral.rs would be the single
path.

## HYG-038 - The determinism list in math.rs and README names functions that do not exist or are exact

Reported by the stdlib hunter.

math.rs's module doc and README "Determinism" (near-identical copies) list
`pow` (no `math.pow` is registered) and `sqrt` (IEEE 754 requires correct
rounding, so it is bit-stable). The list should name what is registered and
ULP-variable (`sin cos tan asin acos atan atan2 exp log`) and live in one place.
Relatedly, which reference version each stdlib function tracks is stated per
site (string_format.rs "5.4 contract", math.rs random "matches 5.4", unpack
"matches Lua 5.2", README's `math.log` note) with no single per-function
statement.

## HYG-039 - Saves embed the crate version, and function identity depends on the host build's code folding

Reported by the surface hunter.

`save_state` embeds `env!("CARGO_PKG_VERSION")`, so identical worlds saved by
0.4.0 and 0.4.1 differ in bytes; the module doc's "two saves of the same logical
state are byte-identical" holds only within one crate version, which matters to
a host that content-hashes saves. Make it optional or document it in the claim.
`register_rust_fn` keys the reverse map on `func as usize`; the comment notes ICF
folding under release LTO but not the consequence: two distinct host functions
folded to one address both save as the smallest id, so after load in a build
that does not fold them, a value that was `b` comes back as `a` and Lua `==`
between them changes across save/load. Document it or refuse when two ids share
an address.

## HYG-040 - The stdlib-or-not decision has two owners

Reported by the surface hunter.

`Engine { install_stdlib }` decides it for `new_state`; `State::load_state`
ignores any Engine and decides from the save's `has_standard_environment` flag.
An `Engine::raw()` user loading a stdlib save gets a stdlib State with no
refusal. If intended, `load_state` should live on `Engine` and check or override
the flag explicitly.

## HYG-041 - Process-global state shared across States

Reported by the vm, compiler and stdlib hunters.

`anchor::NEXT_STATE_ID` is a process-global `AtomicU64` (only Anchor identity
and Debug; documented in anchor.rs). `GMATCH_WRAPPER` (lua_std/string.rs) is a
`static OnceLock<Arc<Bytecode>>` compiled once per process; sound because
`Bytecode` is immutable and caches are per State, but its `source` is `None`, so
tracebacks through `gmatch` name no chunk, and its `Arc` identity is shared
across States, which matters if snapshot or identity logic keys on the pointer.
The hunters found the RNG properly owned and `format_pointer_ids` per-State
deterministic.

## HYG-042 - Native stack depth is an unstated host requirement

Reported by the vm hunter. Already tracked in notes/todo.md ("Depth limits are
not stack-safe in unoptimized builds"); recorded here for the hunt's account.

`MAX_CALL_DEPTH = 1000` is safe only on a large enough native stack. An
embedder calling from a small-stack thread (a 2 MiB worker, or musl's 128 KiB
default) gets an abort, not `CallDepthExceeded`. Nothing detects or documents
the required stack size. Document a minimum and/or make the depth limit
injectable (HYG-010). BUG-004 is the same failure mode with no limit at all.

## HYG-043 - Unbounded or quadratic growth in the VM and registries

Reported by the vm, surface and compiler hunters.

- `format_pointer_ids` (a Vec with linear probe) is pruned only in
  `gc_collect`; with `gc_disable_auto` it grows without bound and each `%p` /
  `tostring` of a table is O(n). Use an `IndexMap<Val, u64>` and/or prune
  independently.
- `open_upvalues` `find_or_create_upvalue` is a linear scan plus sorted
  `insert`, O(n) per capture; bounded by the stack cap but quadratic.
- `Registry` (anchors) is embedder-controlled and unbounded, and `heap_size()`
  does not count it.
- `rust_fns_by_id` and `rust_fn_ids_by_addr` never shrink; no unregister.
- Swept `bytecode_caches` entries keep literals alive one extra GC
  (documented).
- Every nested function clones `source: Option<String>` (`parse_chunk`); make it
  `Option<Arc<str>>`.

# Rules duplicated inside the compiler

## HYG-046 - Codegen sequences copied across the parser

Reported by the compiler hunter.

- "Open the multi-value tail" has five implementations: call args and
  method-call args in expr.rs (two near-identical 25-line blocks), stmt.rs
  `parse_return`, stmt.rs `adjust_multi_assign`, and table.rs, with three
  failure styles (`unreachable!`, `debug_assert!`, `debug_assert_eq!`) and the
  two ArgCount spellings from HYG-002. One `open_tail(&mut self, last:
  &ExpDesc, want: RetCount)`.
- PlaceExp get-lowering is written three times (parser.rs `eval_prefix_exp`,
  func.rs `parse_fndecl_table`, `parse_fndecl_method`) and set-lowering twice
  (stmt.rs `parse_assign`, func.rs `parse_fndecl_basic`); the func.rs copies end
  in `unreachable!`. `parse_fndef_named` and `parse_fndef_method` are the same 15
  lines apart from `params.insert(0, "self")`.
- The comparison-to-fused-branch mapping is in `push_branch_false`, and the
  ordering trick (`<=` is `!>`) is spelled in both vm/frame.rs and
  vm/eval_control.rs; the opcode table in HYG-001 could own it.

## HYG-047 - gmatch snapshot ids are spelled twice

Reported by the stdlib hunter; the magic-byte half was fixed. The gmatch snapshot
ids `"string.gmatch.iter"` and `"string.gmatch.empty_iter"` are also spelled at
the push sites and again in the `register_rust_fn` block; a typo makes a saved
gmatch closure unloadable. Use consts.

# Output channel and lints

## HYG-048 - Library code writes to stdout, and the debug features are never compiled by the gate

Reported by the compiler, vm, surface and tooling hunters.

`debug_parser` prints `println!("Compiled chunk: ...")` in `parse_chunk` - the
raw pre-finalize chunk with NOPs and unassigned cache slots, not the shipped
bytecode. `debug_vm` prints every instruction (vm/frame.rs) and `debug_gc`
prints in vm/object.rs (two sites), all to stdout, interleaving with script
`print` output and bypassing `HostCallbacks`. `brokkr.toml` sweeps only
`default` and `snapshot`, so `debug_parser`, `debug_vm`, `debug_gc`, `hotpath`
and `hotpath-alloc` are never built by `brokkr check`; nothing outside one
AGENTS.md line exercises the debug features, which the tooling hunter reads as
a sign they are dead.

`HostCallbacks::on_print`'s default calls `println!` from library code, and
`DefaultCallbacks` (documented "suitable for CLI tools") is what
`Engine::new_state()` and `State::new()` hand every embedder, so a library
consumer that forgets to override writes to the process's stdout.
`DefaultCallbacks` is an empty impl existing only because the trait has default
bodies. GC collections, budget exhaustion and depth-limit hits produce no
host-visible event besides the error; there is no GC callback.

Fix: route dumps through a host hook, or delete the debug features and add a
`Bytecode` disassembler test helper; make `on_print` required or a no-op
default, with main.rs supplying a `StdoutCallbacks`. Enforceable:
`clippy::print_stdout` / `print_stderr` denied crate-wide, allowed in main.rs
and at feature-gated sites; `[[check]]` entries per feature (a clippy-only pass
would do) or delete the features.

## HYG-051 - `hotpath` is a non-optional dependency of the library

Reported by the tooling hunter.

`hotpath = "0.28.6"` is unconditional and the features only toggle
`hotpath/hotpath`, so every downstream consumer (the game) compiles hotpath and
its dependency tree for no-op macros. Make it `optional = true` with a
`cfg_attr` shim, or record why not. Enforceable: a `cargo tree -e normal` check
in the gate. Related: `update_line` in the parser carries
`#[hotpath::measure]` on a one-line setter called per token, inflating the
parser's profile distribution.

# Tooling and test harness

## HYG-053 - The differential-test policy has residue

Reported by the tooling hunter; mostly fixed (reference-Lua preflight, zero
exit for DIFF files, `DELLINGR_DIFF_TIMEOUT`, `-- SKIP-DIFF:` markers).

- The five corpus skips (`benchmark.lua`, `stress_*.lua`,
  `upvalue_stress.lua`) are still a fallback list in `diff_test.sh` and
  `diff_test.rs`; converting them to `-- SKIP-DIFF:` markers in the files
  retires both lists. Whether the Rust test still skips green on missing
  reference Lua is unverified.
- A match against either lua5.2 or lua5.4 passes the whole file, so a regression
  toward 5.2 behaviour in a 5.4-intended feature is invisible; per-file `-- REF:
  5.4` pinning would sharpen it.

## HYG-055 - CLI output, usage and exit codes are ad hoc

Reported by the tooling hunter.

- `Cost used:` goes to stdout after the script's own prints, even when the
  script errored (printed before the error check); the `--analyze` caveat
  warning also goes to stdout. AGENTS.md says `diff_test.sh` filters the line
  out; it actually passes `--quiet` (as `run_examples.rs` does), while
  `bench.sh` and `hotbench.sh` run without it. The format string lives only in
  main.rs and `cli_limits.rs` asserts it as a substring.
- Messages are ad hoc `println!`/`eprintln!` with prefixes "Error:", "Parse
  error:", "Error reading ...:" and "Unknown option:" (no prefix). Help goes to
  stdout; short usage to stderr, as a second differently worded copy
  (`Usage: dellingr [OPTIONS] <file.lua>` vs `Usage: dellingr [--limit N]
  [--analyze] [--quiet] <file.lua>`).
- `exit(1)` at seven sites in main.rs for usage, IO, parse and runtime errors
  alike; `exit(0)` for help; hotpath.rs uses 2 for usage and 1 otherwise;
  hotbench.sh 2 for usage; diff_test.sh 1. A caller cannot tell "script failed"
  from "invoked wrong" or "budget exhausted", which the game host cares about.
  Fix: an `ExitCode` enum per binary with distinct usage, IO, compile, runtime
  and budget codes, `main` returning it. Enforceable: ban
  `std::process::exit` in main.rs and `examples/` via `disallowed-methods`.
- Argument handling: `std::env::args()` panics on non-UTF-8 (use `args_os`);
  with several positionals the last silently wins; a lone `-` is "Unknown
  option"; no `--` terminator for paths starting with `-`; `--limit` accepted
  and ignored under `--analyze`. Fix: a parser library or a table-driven parser
  with tests, generating usage from one option table.
- `main.rs` installs `hotpath::CountingAllocator` under `hotpath-alloc` but never
  builds a `HotpathGuard`, so nothing reports; its comment is a near-copy of
  hotpath.rs's and has diverged ("through 0.14" vs "0.15"). Delete it or say why
  it is load-bearing.

## HYG-056 - The bench harness has residue

Reported by the tooling hunter; mostly fixed (required path argument, nonzero
exit on missing `_bench` and marker failures, one metric list).

- `O_NONBLOCK` is hand-rolled (`0x800` linux, `0x0004` macos); any other unix is
  a compile error. Use `libc::O_NONBLOCK`, which needs `libc` declared as a
  direct dependency (it is only transitive today).
- The marker protocol (`<us> NAME`, `<us> @k=v`) is defined here and parsed in
  brokkr, with nothing keeping them in step (shared protocol crate or
  brokkr-side schema test).
- `warm_avg_us` is now printed as fractional text (`{:.3}`); unverified whether
  brokkr parses it as an integer.
- The FIFO is opened `O_NONBLOCK` and any write error now exits 1, including
  EAGAIN on a full pipe, so a slow brokkr reader can kill a bench run that
  previously lost markers. Retry on WouldBlock.
- `require_bench` looks up `_bench` inside the timed warm loop, charging a
  `get_global` per iteration to the timing.
- The plain-pattern classifier test re-spells the magic-byte literal instead of
  iterating luapat's constant, so it does not tie the fast path to the grammar.

## HYG-057 - Workload lists and the harness calibration have several owners

Reported by the tooling hunter.

- Three workload registries: `bench.sh`'s default list, `[dellingr.workloads.*]`
  in brokkr.toml, and AGENTS.md's directory table. `bench.sh` uses
  `examples/benchmark.lua` (no `_bench`) where brokkr uses
  `examples/composite/benchmark.lua`, and the README table comes from `bench.sh`,
  so README and brokkr can mean different kernels by "benchmark". `bench.sh`
  could read `hotpath_file` keys from brokkr.toml, or a test could diff the lists.
- `bench.sh` spells `lua5.5 lua5.4 lua5.2 luajit` four times (preflight,
  hyperfine `-n`, jq selectors, header) and says "5 lua versions" for 4
  references plus dellingr. One array. Its hyperfine JSON is deleted afterwards,
  so nothing durable remains unless stdout is copied.
- `DEFAULT_ITERATIONS = 20` and `MAX_WARM_BLOCKS = 32` are restated in AGENTS.md
  and CLAUDE.md ("default 20", "~2s warm phase", "up to 32"), and `bench/` files
  are calibrated against 20. Reword the docs to refer to the constant.
- Hash pins are enforced; the "same kernel in the same proportions" pairing
  between `bench/X` and its examples/ counterpart is not. A test reading
  brokkr.toml could check both files define `_bench` and that their per-call
  `cost` is in a fixed ratio.
- `large_source.lua` is pinned twice in brokkr.toml with the same hash; nothing
  checks it equals `generate.py` output (the hash pin fires on legitimate
  regeneration too). `generate.py` restates the parser's 255 nested-function
  limit as prose and guards `FUNCTION_COUNT = 200` by comment only. A test or
  brokkr script_check diffing generator output would hold it.

## HYG-059 - Smaller test-harness drift

Reported by the tooling and compiler hunters.

- `run_examples.rs` checks stdout only; stderr is unchecked.
- `run_feature_test_extended_in_process` unwraps the load/call result with no
  message.
- `cli_limits.rs` scratch files go to `CARGO_TARGET_TMPDIR` (correct), but in
  `signed_limits...` the `remove_file` follows an assert inside the loop, so a
  failure leaks the file; a drop guard would fix it.
- `compiler_examples_pass_stack_discipline_verification` walks
  `Path::new("examples")` relative to cwd; use `env!("CARGO_MANIFEST_DIR")`.

## HYG-060 - `.gitignore` and repo-root files for tools that are gone or undocumented

Reported by the tooling hunter.

`.plans` (the `plans/` folder is retired), `overnight.sh`, `overnight-logs` and
`.diff_test_*.out` (no producer since scratch moved to `mktemp`) are ignored with
nothing in the tree producing them, which hides whatever later lands under those
names. `.agents`, `.codex` and `.claude` are ignored wholesale, so
`.claude/settings.local.json` changes are invisible to review (possibly
intended). `.review.toml` holds only an `[_audit] id`, is read by nothing in
the tree (presumably the external `review` tool's state), and is listed in
Cargo.toml `exclude`; undocumented tool state in the root.

# Dead code and stale docs

## HYG-061 - Code comments cite audit ids that resolve nowhere

Reported by the vm, surface and stdlib hunters.

`(L5)`, `(L6)`, `(L8)`, `(L10)`, `(L11)`, `(L17)`, `(L18)` appear in eval.rs,
vm.rs, table_ops.rs, lexer.rs, math.rs, table.rs and save_state.rs (two in
`save_state`/`load_state`). They reference a findings list not in the tree -
durable code citing a transient note. The comment text is self-sufficient; drop
the ids.

## HYG-062 - Dead or one-valued code in the compiler

Reported by the compiler hunter.

- `#[allow(dead_code)]` on `Instr::c()` is stale (used by the verifier,
  `RuntimeCaches::new` and `TFOR_CALL` dispatch); removing it lets the `unused`
  deny catch future disuse.
- Unused `_tok_start` parameters on `peek_equals`, `lex_string` and
  `lex_exponent`, threaded through callers.
- `assign_cache_slots` returns `Result<()>` with no error path; the `?` in
  `finalize` reads as a check that never happens.
- `strip_dead_instructions` re-checks `code.len() == line_info.len()` as a
  runtime error right after `finalize`'s `debug_assert_eq!`, and the verifier
  checks it a third time.
- `// TODO: rename to error_here` on `Parser::error`; `parse_if` only forwards
  to `parse_if_arm`.
- The verifier accepts `OP_SET_FIELD_AT` with A=0 and `OP_NOP`, which the
  compiler never ships (finalize rewrites and strips them). Fine for hand-built
  snapshots, but say so in the verifier or reject them so "what the compiler
  emits" and "what the verifier accepts" stay one documented set.

## HYG-063 - Dead or redundant code in the VM, surface and stdlib

Reported by the vm, surface and stdlib hunters.

- `"<function>"`: `RUST_FN_DISPLAY` exists in vm/lua_val.rs but
  `to_bytes_with_heap` in the same file spells `b"<function>"`.
- `bytes_coerce` is a pure alias of `bytes_with_default_string_coercion`.
- String size check duplicated: `check_string_size` (vm.rs) and an inline `len >
  MAX_STRING_BYTES` building the same error in save_state.rs;
  `resolve_bytecode_runtime_no_gc` skips the check (relies on load-time
  validation).
- `ErrorKind::InvalidJump` is constructed only in vm/frame.rs `jump` (and its
  test), reachable only if verification has a bug; an `InternalError` in
  disguise with its own public variant.
- The commented-out alternative in `SyntaxError::is_recoverable` (`//
  matches!(self, Self::UnclosedString | Self::UnexpectedEof)`) records an open
  REPL question; move it to notes/todo.md or delete it.
- `Box<dyn HostCallbacks + Send>` in `Engine::new_state_with_callbacks` and
  `State::load_state` restates a bound `HostCallbacks: Send` already has.
- `base_unpack_values` (basic.rs) and `table_unpack_values` (table.rs) are
  one-line wrappers around `unpack_values`; if they exist for distinct fn
  addresses in the snapshot registry, say so in a comment.
- `analyze_cost` and `Engine::analyze_cost` duplicate the whole body (parse,
  `analyze_chunk` with `"main".to_string()`, wrap); one private helper.
- `LuaPattern::matches_bytes` is `cfg(test)` only (documented, fine).

## HYG-064 - Doc comments that state stale or contradicted facts

Reported by the compiler, vm, surface and stdlib hunters.

- `instr.rs` module doc: "4x smaller than the previous enum-based representation
  (~16 bytes per instruction)" describes a representation that no longer exists.
- `State.globals` doc: "May be changed to an actual Table in the future".
- vm.rs: orphan comment "State marking is done through mark_gc_roots() which
  has direct heap access".
- `load_string_named` doc mentions `"[fleet:123]"`; the host.rs module example
  uses `FleetCallbacks`/`fleet_id` and the pre-`Engine`
  `State::with_callbacks(Box::new(callbacks))` entry point; point it at
  `Engine::new_state_with_callbacks`.
- Public `concat` rejects `n < 2` with an ArgError naming `"concat"`, while its
  doc says n=0 yields the empty string and n=1 is a no-op.
- `patterns/mod.rs` says "wrapper around the vendored Lua pattern matcher"; it
  is an in-crate rewrite.
- error.rs documents `MetamethodDepthExceeded` as covering `__index` and
  `__newindex`; it now covers `__call` too.
- The lib.rs crate-level and `Engine` doc examples are `ignore`d, never compiled,
  and look compilable today; make them doctests.
- `State: Send` has a compile-time witness in lib.rs; `Engine` ("is `Send +
  Sync`") and `Program` ("safe to share across threads") do not. Extend the
  `const _` block with `assert_send_sync::<Engine>()` and `::<Program>()`.
