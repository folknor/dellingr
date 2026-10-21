# Hygiene findings

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

Consolidated from the hygiene hunt at 6c795d1: six Opus hunters, one per scope
(compiler front end, VM execution core, heap/tables/persistence, stdlib and
patterns, host surface/cost/CLI, tooling and harnesses). The question was what
has more than one owner, or rests on an assumption about its host. Hunters read
code and ran nothing. Entries are unranked and unverified; where several hunters
reported one mechanism, that is noted as a fact about the reports, and where they
disagreed both readings are kept. Defects found along the way are in
`notes/bugs.md`.

Common context several entries lean on: the build can enforce clippy lints and
`clippy.toml` path bans, visibility, types, golden tests, and the gremlins scan.
It has no custom text rule beyond gremlins and no module dependency-direction
rule, so many "enforce by grep" proposals below also imply adding such a check
to `brokkr check`.

---

## Encodings and operands

## HYG-004 - Two opcode-to-function tables for arithmetic

Reported by the compiler hunter.

`numeral::arith_fn` maps opcodes to `lua_add`...`lua_modulo` for the folder;
`vm/frame.rs` dispatch arms map the same opcodes by hand. The "bit-exact by
construction" comment rests on two hand-written tables agreeing. Fix: `const fn
arith_fn`, dispatch calls `const { arith_fn(Instr::OP_ADD) }`, or carry the
function in the `define_opcodes!` row. Enforceable structurally only; a test
comparing the two cannot be written.

## HYG-005 - Snapshot environment token names spelled at more than one site

Reported by the VM-core hunter.

`capture_env_tokens` names one token through `STRING_METATABLE_TOKEN` and another
as the inline literal `"_G.metatable"`; builtin tokens come from `slot.name()`;
the loader in `save_state` must spell the same strings. Fix: one token table
owned by save_state.

---

## Limits, tunables and where they live

## HYG-006 - No single home for limits and tunables

Reported by the stdlib, host-surface, heap and VM-core hunters.

`limits.rs` claims to be the home of fixed resource and encoding limits and says
the snapshot format validates against them, but replay-visible limits live
elsewhere:

- `patterns/luapat.rs`: `MAXCCALLS = 200` (`MatchDepthExceeded`),
  `LUA_MAXCAPTURES = 32`.
- `string_format.rs`: `MAX_FORMAT_LEN = 22` with no note that it is lstrlib's
  `MAX_FORMAT - 10`; width/precision digit cap is a bare `> 2`.
- `basic.rs`: `tonumber` base range `2..=36` inline.
- `table.rs`: `MAX_EXACT_KEY` (see HYG-069); `sort_through_metamethods`'s
  `len >= i32::MAX` "array too big" (see HYG-070).
- `save_state.rs`: every `MAX_*_BYTES` codec cap (see BUG-008 in
  `notes/bugs.md` for the enforcement gap).
- `vm/object.rs`: GC thresholds (see HYG-008).
- `compiler/parser/table.rs`: inline-template threshold `4` (HYG-016); the
  doubled syntax-level charge per call/index suffix (HYG-017); `numeral.rs`
  `MAX_SIGNIFICANT_HEX_DIGITS` and the `+2048` exponent clamp (correctness
  parameters, not tunables, but undocumented as such).
- `vm.rs` capacity hints (HYG-007) and `Vec::with_capacity(256/64)` in
  `empty_with_callbacks`; `HOST_GLOBAL_CAPACITY_HEADROOM`.

Conversely `limits.rs` holds non-limits as `pub`: `COST_CHECK_INTERVAL` (a
batching knob `set_cost_budget` promises is unobservable) and
`GMATCH_PATTERN_CACHE_*`, advertising internals as contract.
`Frame::COST_CHECK_INTERVAL` re-aliases the limits constant with its own
docstring (VM-core and host hunters). Three independent `200`s
(`MAX_SYNTAX_DEPTH`, `MAX_METAMETHOD_DEPTH`, `MAXCCALLS`) look related and are
not; `MAXCCALLS` is reference-pinned. The VM-core hunter judged `limits.rs`'s "no
injection point" rationale (replay contract) a real answer, not a finding.

Fix: `limits` (public, script-observable caps, everything that produces an
`ErrorKind`, with a "reference-pinned library limits" heading) plus a
`pub(crate) tuning` module (batch interval, cache sizes, GC thresholds, capacity
hints). Drop the `Frame` alias. Enforceable: the module split itself; not by
lint.

## HYG-007 - Stdlib capacity counts are hand-maintained in two modules, two ways

Reported by the VM-core, heap and stdlib hunters.

`lua_std.rs`: `MATH_LIBRARY_CAPACITY = 22`, `STRING_LIBRARY_CAPACITY = 10` are
hand counts while `TABLE_LIBRARY_CAPACITY = TABLE_FUNCTIONS.len()` is derived.
`vm.rs`: `STDLIB_INTERNED_TABLE_KEYS = 41` (22 + 10 + 7 + `__index`/`__newindex`),
`STDLIB_HEAP_TABLES = 6`, `STDLIB_NON_BUILTIN_GLOBALS = 1`, each "keep in step
with lua_std". The stdlib hunter counted and found them in agreement today; drift
costs a reallocation only, which is why nothing would notice.

Fix: `MATH_FUNCTIONS` / `STRING_FUNCTIONS` slices like `table.rs`, every count
derived (VM constants become const sums or go). Interim: a test opening the libs
and asserting counts. Group the other `vm.rs` capacity literals next to them.
Enforceable structurally once counts are `.len()`.

## HYG-008 - GC policy: constants unlisted, initial threshold not injectable, host threshold lost on load

Reported by the heap and VM-core hunters.

- `GC_MIN_THRESHOLD = 20`, `GC_INITIAL_THRESHOLD` (`= GC_MIN_THRESHOLD`, used
  once, a second name suggesting a past difference) and
  `GC_THRESHOLD_GROWTH_FACTOR = 2` sit at the top of `object.rs`. `limits.rs`
  does not mention them and no `docs/` page describes collection policy.
- `GcHeap::with_threshold` is `cfg(test)`; a State cannot be built with a chosen
  initial threshold, and `open_libs` already ran under the default by the time a
  host can mutate it. The initial threshold is below the stdlib's footprint, so
  every `State::new` collects during `open_libs`.
- Host-set thresholds are one-shot: every `collect` overwrites `Automatic(_)` with
  `survivors * 2` (documented in rustdoc).
- `load_state` runs the host `setup`, then `materialize_payload`, which ends with
  `gc_collect()`, replacing any threshold setup configured; only `Disabled`
  survives. The threshold is not in the snapshot, and nothing says so.

Fix: document in `docs/save-state.md`, or make GC policy explicit `State` config
(initial, growth, min) that collection reads rather than overwrites. Drop or give
a role to `GC_INITIAL_THRESHOLD`. Enforceable by a test: set a threshold in setup,
load, assert it.

## HYG-009 - `MAX_STRING_BYTES` and `MAX_RESULTS` reachable through several paths; string-size policy lives in `vm`

Reported by the compiler and host-surface hunters.

`MAX_STRING_BYTES` is `limits::`, re-exported from `vm.rs`, re-exported again
from `lib.rs`; the parser uses `crate::vm::MAX_STRING_BYTES`,
`crate::vm::check_string_size` and `checked_string_growth`;
`save_state/verify.rs` uses `super::super::MAX_STRING_BYTES`. `MAX_RESULTS` is
both `limits::` and `error::`. The check functions live in `vm`, so the compiler
depends on the VM for a limit check. Fix: one public path (`limits::`), move the
check functions beside the constant, drop the re-exports. Enforcement is deleting
them (no lint catches const re-exports); a dependency rule (HYG-027) holds the
compiler side.

## HYG-010 - `MAX_SOURCE_BYTES` silently underwrites narrow widths and equals `MAX_STRING_BYTES` by coincidence

Reported by the compiler and host-surface hunters.

The lexer's `tok_line = linebreaks.len() as u32`, `Token.len: u32`, and
`CallSite`'s 24-bit line field (`LINE_MASK`) are sound only because
`MAX_SOURCE_BYTES = 16 MiB`; each has a prose comment, nothing ties them. Raising
the cap above 2^24 makes tracebacks saturate line numbers with no test failing.
`MAX_SOURCE_BYTES` and `MAX_STRING_BYTES` are separate `16 * 1024 * 1024`
literals; a reader cannot tell if they are one policy. If they diverge, `load` of
a max-length string starts refusing strings the VM built (`SourceSizeExceeded`).
`GMATCH_PATTERN_CACHE_BYTES = MAX_STRING_BYTES` already shows the derived style.

Fix: `const _: () = assert!(MAX_SOURCE_BYTES <= CallSite::LINE_MASK as usize)`
and a `u32::MAX` assert; decide explicitly whether one is defined as the other.
Enforceable by compile-time asserts.

## HYG-011 - Public line-number widths disagree

Reported by the host-surface hunter.

`Error::line_num: usize`, `Error::column: usize`, `StackFrame::line: u32`,
`on_print(line: u32)`. `limits.rs` says source size is capped so lines fit u32.
Pick `u32` on the public surface. Enforceable by type.

---

## Cost model

## HYG-012 - Budget state has two representations and is not validated on restore

Reported by the host-surface and VM-core hunters.

`State` carries `cost_budget: i64`, `cost_remaining: i64`,
`cost_budget_configured: bool` and `cost_used`. "Unbounded" is the bool and the
`i64::MAX` sentinel; `cost_remaining()` re-spells `i64::MAX` when the bool is
false; `CHANGELOG.md` documents that return. Consumers must test the bool first:
`cost_meter`, `cost_batch_needs_flush`, `gmatch_subject_matcher_and_cost_meter`
(hand-destructures the fields), `lua_std/table.rs` sort
(`cost_budget_configured || honour_meta`). Already diverged: verifier and
save_state fixtures build `0/0/false`, a combination `State::new` never produces.
Restore copies all four fields without checking consistency (`remaining >
budget`, configured=false with non-MAX remaining), so a hand-built or corrupted
snapshot can install a meter state no API reaches. The bool only goes
false->true; there is no way back to count-only, and `set_cost_budget` does not
say whether that is intended. Lateral: negative budgets are accepted (tested);
`BudgetExceeded { budget: i64, used: u64 }` mixes signedness.

Fix: one `budget: Option<Budget { limit, remaining }>` (or an owned `CostState`
that hands out `CostMeter`), `cost_remaining()` a single `map_or`; snapshot
encoding follows the type (tag byte + payload). Enforceable by type.

## HYG-013 - Opcode charges restated in dispatch comments; stdlib charges have no list

Reported by the VM-core and stdlib hunters.

`frame.rs` arms carry "costs 1", "Table reads are free", "comparisons are free";
the owner is `Instr::static_units()`, read correctly via `const { .. }`, so the
comments (and presumably `docs/budget.md`'s table) are second copies. On the
stdlib side about 40 cost literals are deliberate (the `string.rs` comment
explains why a rule table would not help `analyze_cost`), but nobody can answer
"what does `string.gsub` charge" without reading code. The formula
`(s.len() + pattern.len()).max(1)` plus `charge_bytes(pattern.len())` repeats in
find, match and gsub while gmatch charges the second part lazily. `math.rs`
repeats `consume_cost(1)` 20 times on the same reasoning. `print` charges nothing
though its work scales with output; consistent with the budget stance, but absent
from the `string.rs` list of deliberately free functions.

Fix: delete numeric claims from dispatch comments; generate or test the budget.md
table from `static_units`; a golden test snapshotting `cost_used` per library
function on fixed inputs, which turns "needs a version bump" into a check and
supplies the missing charge table.

## Compiler front end

## HYG-016 - Table inline-template threshold spelled twice

Reported by the compiler hunter.

`table.rs` uses `field_count > 4` in `parse_table` and `[TableTemplateField; 4]`
plus `self.inline.len()` in `TableTemplateCandidate`. Changing one without the
other makes `fields_for_template` silently return `None` for 5..N fields. Fix:
`const INLINE_TEMPLATE_FIELDS`. Enforceable by the const, or a test that a
5-field named constructor gets a template.

## HYG-018 - Expandable-tail predicate spelled three times

Reported by the compiler hunter.

`Parser::open_tail`, `adjust_multi_assign` (stmt.rs) and `parse_table_entry`
(table.rs) each match `ExpDesc::Prefix(PrefixExp::FunctionCall(_)) |
ExpDesc::Vararg`; `open_tail`'s `assert_eq!` and two caller `assert!`s exist only
because of it. Fix: `ExpDesc::is_multi`, or a `MultiTail(TailKind)` variant that
`open_tail` takes by value, removing the asserts. Enforceable by type.

## HYG-019 - Two styles for narrowing to u8 against a limit

Reported by the compiler hunter.

`checked_fixed_arg_count`, `checked_multi_assign_width`, `parse_concat`,
`parse_assign` (`MAX_ASSIGNMENT_OFFSET`), `parse_chunk` (params) and
`parse_generic_for` compare against a `limits::` const then do an infallible
`u8::try_from(..).map_err(same error)`. Elsewhere bare `as u8` (`locals.len() as
u8` five times in stmt.rs, `local_idx`/`upvalue_idx as u8` in upvalue.rs,
`nested.len() as u8 - 1`, `template_idx as u8`) is sound only because an earlier
check bounds the value. Fix: one `bounded_u8(value, limit, err)`, and/or a
`Slot(u8)` newtype only `add_local` constructs. Enforceable:
`clippy::cast_possible_truncation` / `as_conversions` scoped to `compiler/`
(`lua_std.rs` already denies `cast_possible_truncation`).

## HYG-020 - Number-literal validation split between lexer and `numeral.rs`

Reported by the compiler hunter.

Hex needs mantissa and exponent digits in the lexer, but a decimal exponent with
zero digits (`1e`, `1e+`) passes `lex_exponent` and is rejected by
`numeral::parse_decimal`. Same `BadNumber` error, two enforcement points. Hex
digits are classified by `is_ascii_hexdigit` in the lexer and
`numeral::hex_value` in the parser. Fix: the lexer finds the maximal numeral span
(Lua's `read_numeral`), `numeral::parse_lua_numeral` is the single validator.
Enforceable by a test running every lexer fixture through both paths.

## HYG-021 - String-literal bodies scanned twice; newline-pair consumption written three times

Reported by the compiler hunter.

`lex_string` scans to the closing quote with its own `\z` and escaped-newline
handling; `Parser::get_literal_string_contents` re-scans to decode,
re-implementing both. `is_newline_pair` is shared but the consumption rule is
written in `Lexer::next_char`, `lex_string` and the parser decode. Errors split:
lexer reports unclosed strings, parser reports bad escapes. Fix: decode once in
the lexer and hand decoded bytes to the parser. Enforceable structurally; today
only the test suite keeps them in step.

## HYG-024 - Line attribution depends on which token-consumption helper a site used

Reported by the compiler hunter (not verified against reference line numbers; a
probe is warranted).

`current_line` updates only in `Parser::expect` and at statement start; `next`,
`try_pop` and `check_type` leave it. In `local x = a +\n b +\n c` the arithmetic
ops carry the statement's line, and a `)` taken by `expect` on a later line moves
everything after it. Calls work around it with an explicit `CallSite` line.
Affects runtime error lines (operator-facing, not replay-visible). Fix: update the
line in `TokenStream::next` and stamp operators with the operator token's line
(`luaK_fixline`). Enforceable by a line-info golden test over multi-line
expressions.

## HYG-025 - Syntax-depth bracket hand-rolled at each recursive entry

Reported by the compiler hunter.

`enter_syntax_level()?; let r = inner(); exit_syntax_level(); r` in
`parse_statements`, `parse_expr`, `parse_unary`, `parse_if_arm`, doubled in
`parse_prefix_extension`. An early `?` between the calls leaks depth silently.
Fix: `with_syntax_level(cost, f)`. Enforceable if `syntax_depth` is touched only
inside it, backed by a grep.

## HYG-026 - Call-site marker emission duplicated across two suffix arms

Reported by the compiler hunter.

The `LParen | LiteralString | LCurly` and `Colon` arms of `parse_prefix_suffix`
both emit `mark_call_base(1)`, parse args, call `open_tail`, NOP the marker for
fixed counts and build a `CallSite`, with copied comments; they differ only in the
prelude and implicit-arg count. Fix: one `finish_call(mark_idx, implicit_args)`.

## HYG-027 - Compiler depends on the VM; runtime cache types and `unsafe impl Sync` live in the compiler

Reported by the compiler hunter.

`compiler.rs` imports `vm::{ObjectPtr, Val}` to define the
`*LookupCacheEntry`/`*Slot` types, `TforCursorEntry` and `RuntimeCaches`, none of
which is compiler output; every consumer is in `vm/`. The parser also calls
`crate::vm::check_string_size` (HYG-009). `unsafe impl Sync for RuntimeCaches`
argues "every access goes through `&mut State`", but `RuntimeCaches` sits in
`Arc<BytecodeRuntime>`, cloned into heap closures and frames; soundness rests on
no `Arc<BytecodeRuntime>` escaping the State, held by convention.

Fix: move the cache types to `vm/caches.rs` (compiler exposes slot counts only);
keep `BytecodeRuntime` private to `vm` and state the real obligation in the
SAFETY comment, or replace `Sync` with a Send-only handle. Enforceable: a
`brokkr check` grep that `src/compiler/**` contains no `crate::vm`/`super::vm`
(nothing like it exists); partly by visibility.

## HYG-028 - Literal pools are a two-writer invariant with doubled string storage

Reported by the compiler hunter.

`chunk.string_literals`/`number_literals` and `literal_indices.strings`/`numbers`
must stay in step; `find_or_add_*` writes both, and the string map stores a
second full copy of every string (doubling pool memory up to the source cap).
Code that pushes to a pool directly desynchronizes them silently. Fix: an
`IndexSet<Vec<u8>>` (via det_hash) where index = position, converted in
`finalize`; numbers keep a key newtype. Enforceable structurally.

## HYG-029 - Fold-failure fallback policy spelled twice

Reported by the compiler hunter.

`push_arith` and `push_negate` both `if let Ok(idx) = find_or_add_number(..)`,
dropping `TooManyNumbers` and falling back unfolded. Correct (folding must not
reject a valid program), but the rule lives in two places. Fix: one
`try_fold_literal(value) -> Option<u16>` owning NaN, -0.0 and full-pool policy.

## HYG-030 - Compiler invariants panic while `finalize` argues they must be errors

Reported by the compiler hunter.

`finalize`'s comment states a compiler bug must surface as a catchable
`InternalError`, never a host panic. The parser contradicts it at roughly fifteen
sites: `assert_eq!` in `get_literal_string_contents` and `open_tail`; `assert!`
in `adjust_multi_assign` and `parse_table`; `assert_eq!(nest_level, 0)` in
`parse_all`; two `panic!` in `peek_equals`; `unreachable!` in
`parse_fndecl_basic` and `numeral::arith_fn`; `expect` in `exit_loop`,
`add_break`, `replace_last_instr`, `finalize` (`Arc::get_mut`),
`strip_dead_instructions`; `ArgCount::to_u8`/`RetCount::to_u8` asserts via
`Instr::call`/`ret`. None is reachable from script input today. Fix: decide the
rule; several disappear structurally (HYG-018's `MultiTail`, a typed
`CmpTokenKind` for `peek_equals`, splitting `PlaceExp`). Enforceable:
`clippy::panic`, `unreachable`, `expect_used` scoped to `compiler/`.

## HYG-031 - Compile-time validation errors rendered at two sites; metadata failures located at 0:0

Reported by the compiler hunter.

`compiler::finalize` builds "compiler emitted invalid bytecode in function 'x' at
instruction N: reason" by hand; `save_state/verify.rs` builds its own
`invalid(chunk, instruction, reason)`, with different subjects (function name vs
chunk index). When `instruction == None`, `finalize` uses `unwrap_or(0)` and
`with_location(line, 0)`; line 0 column 0 is not "no location", and
`without_location` exists. Fix: `impl Display for BytecodeValidationError` owned
by verify.rs; `without_location` plus source name.

## HYG-032 - Bytecode verification split by data type: tree checks run only on snapshot load, and `SavedBytecode` mirrors `Bytecode`

Reported by the compiler and heap hunters.

`compiler::verify::validate_bytecode` checks each chunk alone. Tree-level checks
(child `UpvalueDesc::Local(slot)` within the parent frame, `Upvalue(i)` within the
parent's count, depth vs `MAX_CHUNK_NESTING`) exist only in
`save_state/verify.rs::validate_nested_graph` over `SavedBytecode`; `finalize`
never runs them, so an upvalue-resolution bug ships unverified on the compile
path. Separately, `SavedBytecode` mirrors `Bytecode` field for field at four sites
(struct, `encode_bytecode` conversion, an all-zeros placeholder literal,
`build_bytecode_node`) plus the `BytecodeView` impl, which does not catch a new
field; `SavedUpvalueDesc` mirrors `UpvalueDesc`.

Fix: move tree checks into `compiler/verify.rs` behind `BytecodeView` and call
from both `finalize` and `verify_payload`; have the codec encode/decode
`Bytecode` directly and verify the decoded value, deleting `SavedBytecode` and its
conversions. Enforceable structurally; the examples-corpus test then covers the
tree checks.

## HYG-034 - Dead or vestigial front-end code

Reported by the compiler hunter.

- `GlobalLookupCacheSlot`, `FieldLookupCacheSlot`, `SetFieldLookupCacheSlot` have
  hand-written `Clone` ("cloned chunks start cold", stale) and an always-true
  `PartialEq`, from when caches lived in `Bytecode`. `RuntimeCaches` derives
  neither and no slot is cloned or compared; `TforCursorSlot` has neither. The
  compiler would reject a slot clone if removed, so deletion is self-checking.
- `TokenType::LiteralHexNumber`: every consumer treats it as `LiteralNumber`
  (`spelling`, `parse_expr_base`, `describe_token`, `is_expr_start`), and
  `parse_lua_numeral` re-detects `0x`. Delete.
- `numeral::scale_finite_by_power_of_two`: its only caller runs at
  `top_exponent >= MIN_NORMAL_EXPONENT` and rounding only raises the exponent, so
  the `< HALF_MIN_SUBNORMAL_EXPONENT` and `< MIN_NORMAL_EXPONENT` branches look
  unreachable (subnormals go through `round_hex_subnormal`). Confirm with
  coverage before deleting: numeric, replay-visible. If dead, it is an untested
  second subnormal algorithm that disagrees in method with the live one.

---

## VM execution core

## HYG-036 - Global read/write rule implemented by four independent writers

Reported by the VM-core hunter.

The rule (builtins live in `builtins[]` mirrored into `globals`; nil to a
never-defined name is a no-op while an existing name keeps a tombstone; a builtin
rebind bumps `globals_version`) is implemented in `vm.rs::set_global_value_owned`
(the documented central setter), `eval_index.rs::instr_set_builtin` (restates the
tombstone rule, bumps the version unconditionally, correct only because every
caller is a builtin), `eval_store.rs::instr_set_global` (slow path writes
`globals` in place for existing non-builtin names) and `try_set_global_cached`
(writes nil in place, the reason tombstones exist). `globals_version` is bumped in
three places (`with_restricted_env` twice). The read side ("`Builtin::from_name`,
then `globals`") repeats in `get_global`, `instr_get_global`,
`with_restricted_env`, and probably the `_G` proxy in `basic.rs` (not verified).
None disagree today. Lateral: `instr_set_builtin` does
`globals.insert(builtin.name().to_string(), val)`, allocating a String per
builtin assignment; use `get_mut` first.

Fix: a `GlobalEnv` type owning `globals`, `builtins`, `globals_version` with
private fields. Enforceable by module privacy, plus a debug assertion at GC mark
that builtin slots equal their `globals` entries.

## HYG-037 - Metamethod event names interned by string literal at each dispatch site, with three error/allocation policies

Reported by the VM-core and heap hunters.

`"__index"` (`metamethod.rs` x2, `eval_index.rs` x2, `vm.rs`, `basic.rs`),
`"__newindex"` (`metamethod.rs`, `basic.rs`), `"__call"` (`eval.rs`),
`"__len"` (`eval_store.rs`, `table.rs` stdlib), `"__tostring"` (`table_ops.rs`
twice, once `b"__tostring"` for `find_string`, once for `alloc_string`),
`"__metatable"` (`basic.rs` x2). A typo compiles and never dispatches. Each
alloc site does its own rooting dance; error policy differs (`?` in
`metamethod.rs`/`index_string`, `.expect(..)` in `eval.rs`, `instr_length`,
`protected_index_key`), and `string_tostring_handler` probes with the
non-allocating `find_string` while `metatable_tostring_field` allocates (may GC).
Every metatable miss on a read allocates and may collect.

Fix: a `MetaEvent`/`MetaName` enum with names interned once per State as roots in
`mark_gc_roots`, read as `meta_names[MetaName::Index]`: removes dispatch-time
allocation, rooting dances and the expect/`?` split. Changes GC timing (and `%p`
ids), deterministic but snapshot-visible, so call it out; see HYG-038 on whether
timing is contract. Enforceable by a text rule banning `alloc_string("__` /
`"__[a-z]+"` literals outside the enum's module.

## HYG-039 - `InternalError(String)` assembled ad hoc; `InvalidJump` is a second "VM bug" kind

Reported by the VM-core and host-surface hunters.

About 20 VM sites write free text ("SetList(0): no constructor base", "call-base
marker is below the active frame", "unknown opcode: N", "InitField: expected
table, got {typ}"); none carries opcode, ip or chunk as data; `locate_in_frame`
adds a line only. `ErrorKind::InvalidJump { ip, offset }` also means "compiler bug
or corrupt bytecode" and hand-copies the `"internal error: "` prefix. Fix:
`ErrorKind::VmInvariant { opcode, ip, what: &'static str }` (or
`InternalDetail::InvalidJump {..}` etc.) with one prefix owner, filled at the
frame boundary. Enforceable by type.

## HYG-040 - VM panics on bytecode-controlled conditions; defensive checks applied inconsistently

Reported by the VM-core hunter.

Safe only because the verifier ran, with nothing in the types saying so:
`OP_SWAP` `stack.swap(len-1, len-2)` underflows; `OP_DUP` `.expect`; `pop_val`
`expect("VM bug")` (OP_POP, OP_EQUAL, every binary op); unchecked
`stack[base + n]` in `instr_set_local`/`get_local`/`tfor_*`; unchecked
`frame.upvalues[idx]`, `bytecode.nested[i]`, `number_literals[i]`,
`string_literals[n]`. If the snapshot load path ever skips verification for a
chunk (not verified), a corrupt save becomes a host panic where `LoadError` was
owed. Meanwhile `Frame::jump` and `instr_set_list_count` keep defensive checks "so
a verifier regression returns a VM error". The unreachable
`"fused branch dispatched with a non-fused opcode"` and `"unknown opcode"` errors
belong to the same question. Fix: a `VerifiedBytecode` newtype only the verifier
constructs, `Frame` accepts only that; then drop defensive checks everywhere or
keep them everywhere. Enforceable by signature.

## HYG-041 - Partial functions whose types admit inputs they panic on

Reported by the VM-core and heap hunters.

`Val::hash` panics on NaN (all callers filter first, enumerated in a comment);
it is a `Hash` impl, so reachable by any future `IndexMap<Val, _>` user.
`to_bytes_with_heap` (`pub(super)`) is `unreachable!` for 2 of 6 variants.
`format_pointer_id` only `debug_assert!`s its input. `GcHeap::get`/`get_mut`/
`StringPool::get` `expect` on stale pointers (the intended tripwire), but
`table.rs`'s tombstone note ("the key of a dead slot may dangle ... must never
dereference it") is the only thing stopping a new raw-slot walker that forgets the
filter from aborting the host. Fix: a `TableKey` newtype built only through
`normalize_table_key` as the only `Hash` implementor; a `Primitive` view or
`Option` for `to_bytes_with_heap`; an iterator API that never yields dead slots.
Enforceable by type.

## HYG-043 - Outermost-host-boundary predicate restated three times in `call_op`

Reported by the VM-core hunter.

`report_call_error`, the inline `host_error` in the `__call` chain block, and a
third inline copy for "not callable" all restate `call_depth == 0 &&
call_chain_depth == 0 && active_rust_fn.is_none()`. The two inline copies should
call `report_call_error`, or a shared `is_outermost_host_boundary()` where
`handler_is_function` is needed.

## HYG-044 - Call-site recording and cost flushing placed by hand per opcode

Reported by the VM-core hunter.

`record_call_site` is called before `OP_CALL`, `TFOR_CALL`, `LENGTH`,
`GET_FIELD`, `GET_TABLE`, `SET_FIELD`, `SET_FIELD_AT`, `SET_TABLE`: any opcode
that can re-enter Lua. `flush_local_cost` is called by hand in each metamethod
helper (`metamethod.rs` x4, `eval_index.rs`, `eval_store.rs`). A new opcode that
forgets either gets a wrong traceback line or a `cost_used` that depends on the
batch interval (replay-visible). Fix options: `State::call` flushes itself; record
the ip into `CallInfo` unconditionally on every call out (one store); or an
`Instr::may_reenter()` table cross-checked by a test. Enforceable by a test running
each opcode against a metatable'd operand asserting traceback line and
`cost_used`.

## HYG-046 - Duplicated small VM routines

Reported by the VM-core hunter.

- `instr_set_list_count` and `instr_set_list` repeat the same three SetList(0)
  base checks with identical strings; extract `constructor_base()`.
- `State::error(&self, kind)` ignores `self` and equals
  `Error::without_location(kind)` ("public compatibility helper"); VM code uses
  both spellings. Pick one inside the crate.
- `concat` checks `n > stack.len() - stack_bottom`, `get_top()` spelled
  differently from every other count-taking API.
- `get_index_table_field_direct` builds `MethodLookupCacheEntry` four times
  differing in two fields; a constructor removes ~30 lines and the risk of one
  copy missing `globals_version`.
- `State::get_string_constant` forwards to `frame.literal(i)` and carries
  `#[hotpath::measure]`; inline it at its 4 sites (see HYG-099).
- `Frame::new`'s `let ip = 0;` and `Frame::bytecode()` vs direct field access:
  trivial.

## HYG-047 - Stack indices held across user code; per-frame bases stored globally

Reported by the VM-core hunter.

`instr_set_field` keeps a stack index across a `__newindex` call, checked only by
`debug_assert_eq!(stack[idx], tbl_val)` ("no current path" safety); in release a
violation removes the wrong slot. Same in `handle_index_metamethod_inner` (reads
`stack[table_idx]` after a flush) and `instr_tfor_call` (writes `stack[base + 3 +
i]` after arbitrary code). Safety rests on called frames living above
`stack_bottom`, structural for Lua frames and true for RustFns only because
`convert_idx` refuses indices below `stack_bottom`; state it as the invariant with
a test. `vararg_call_bases` / `table_constructor_bases` are shared across frames,
balanced by bytecode and repaired by watermarks; per-frame storage would make
cross-frame confusion unrepresentable and delete the watermark logic and three
`InternalError` checks.

## HYG-048 - Public stack API swallows invalid indices; `table_len` does too

Reported by the VM-core and heap hunters.

`stack.rs`: `to_boolean(idx)` returns `false`, `typ(idx)` returns `Nil`,
`raw_equal(a, b)` returns `false` for invalid indices, while `to_number`,
`to_bytes`, `copy_val`, `insert` return `InvalidStackIndex`. Mirrors the C API's
"acceptable index", but inconsistent within this API. Make them `Result` or add
`try_` forms. `table_len` returns 0 for a bad index; `table_insert_at` /
`table_remove_at` call it with the original `table_idx` after
`table_operand_below_top` already resolved it. Pass the resolved index and drop
the fallback. Enforceable by signature.

---

## Heap, tables, persistence

## HYG-050 - The "integer key in 1..=n" rule spelled seven times in three dialects

Reported by the heap hunter.

`Table::array_slot` (`n >= 1.0 && n <= len as f64 && n == n.floor()`),
`is_array_append_key` (`== (len + 1) as f64`), `Table::rebuild` (bit-exact),
`save_state::runtime_array_slot` (`fract().to_bits() << 1 == 0`),
`verify.rs::validate_environment_deltas` (the `fract` form twice plus bit-exact
append and order tests), `validate_references` (`SavedVal::Num` equality twice),
`gmatch_integer_field` (non-negative integer). They agree on every input today
only because array keys are `>= 1`; the verifier is where disagreement becomes
"valid save rejected" or "forged shape accepted". Fix: one `array_index(n, len)`
and `is_append_key` in `table.rs` on `f64`, imported by the verifier as it
already imports `normalize_table_key`. Enforceable by structure and a grep for
`fract().to_bits()` outside `table.rs`.

## HYG-051 - Val-to-table-key mapping written three times; nil/NaN refusal written five times

Reported by the heap hunter.

`normalize_table_key` is the documented owner, but its input mapping is
re-spelled in `Table::insert`, `save_state::runtime_table_key`,
`verify::resolve_key`, and `Val::hash` (calls it with `()` payloads then panics on
branches the type admits). The nil/NaN refusal is hand-written in
`Table::hash_get`, `get_with_index`, `next_with_index`,
`next_from_matching_index`, and `State::set_table_raw` (which pre-checks only to
get a located error, see HYG-053). Fix: `Val::table_key() -> Result<
NormalizedTableKey<..>, InvalidTableKey>` plus a `SavedVal` twin; make
`TableKeyInput` private to `table.rs`.

## HYG-052 - gmatch matcher ordering invariant and env-token schema have no structural owner

Reported by the heap and stdlib hunters; narrowed in wave 1 (ids and keys now
live in `src/gmatch_schema.rs`, reads are strict, the dead branch is gone).

- Ordering invariant: `gmatch_subject_matcher_and_cost_meter` depends on
  `memoize_gmatch_pattern` running immediately before with no allocation between,
  guarded by an `InternalError`; a single `with_gmatch_matcher(.., |subject,
  matcher, meter| ..)` would make it structural.
- The `_G` proxy metatable and string metatable are likewise library-installed
  state the VM tracks as env tokens without a schema owner.

- No test saves and reloads a live gmatch iterator.

Fix: `with_gmatch_matcher`; a schema owner for the env tokens; the save/reload
test.

## HYG-053 - Table and snapshot errors shed their location and reason

Reported by the heap hunter.

`Table::insert` returns `Error::new(TableKeyNil/TableKeyNan, 0, 0)` and
`Table::rebuild` returns `InternalError` the same way; callers propagating with
`?` (`set_table_str_key_value`, `set_list`, `table_ops` dispatch) hand the
operator an error naming no line, which is why `set_table_raw` pre-checks. In
materialization, `rebuild_from_snapshot(..).map_err(|_| corrupt_arena(..))` and
five `table.insert(..).map_err(|_| ..)` sites discard which rule failed.
`LoadError::CorruptArena { reason, index }` does not record which arena the index
belongs to: env-delta checks report `key_index` or `delta_index` inconsistently,
`validate_metatable` reports `0` meaninglessly, `materialize_payload` mixes them
for the same delta. Fix: `Table` returns its own `InvalidTableKey` and the State
layer adds location (removes `Error` as a table-module dependency);
`CorruptArena` carries a typed location (`Object(u32) | EnvDelta { token, entry }
| Global(name) | ..`) and the inner reason. Enforceable structurally (`table.rs`
cannot import `Error`) and by type.

## HYG-054 - Snapshot in-flight inventory and load-time reset are two hand-maintained lists

Reported by the heap hunter.

`State::validate_quiescent` enumerates 12 fields that must be empty;
`materialize_payload` clears 14 (`transient_roots.values` and `.suspended_envs`
separately where the validator calls `is_empty()`); `State::new` is a third
writer. The validator's comment asks to keep the inventory updated by memory. Fix:
an `InFlight` struct with `is_quiescent()` and `reset()`, destructured without
`..` so a new field fails to compile. Enforceable by type.

## HYG-056 - GC allocation and collection protocol held by comment

Reported by the heap hunter.

Every `GcHeap::alloc_*` carries the same "caller must check `is_full()` ...
except snapshot materialization" doc, and `State::alloc_table`,
`alloc_table_with_capacity`, `new_table_with_template` call `maybe_collect()` by
hand. `GcHeap::collect` is `pub(super)` with "gc_collect is the only production
caller" as the guard; a full collection is clear gmatch cache, mark roots, mark
open-upvalue slots, drain, sweep bytecode caches, sweep heap, sweep upvalue pool,
prune `format_pointer_ids`. `mark_gc_roots` claims to be the single source of
roots, but open-upvalue slots and `pending_bytecode_caches` are rooted in
`gc_collect`. Fix: a State-level allocator that always collects plus a
`NoGcAllocator<'a>` borrowed from `&mut GcHeap` only materialization can
construct; heap `collect` private to a collector module owning the side sweeps;
fold the extra roots into `mark_gc_roots`. Enforceable by visibility.

## HYG-057 - `Table` invariants kept by many writers, unchecked

Reported by the heap hunter.

`dead_count` is adjusted by `insert`, `hash_remove`, `init_at_index` (both
directions), `compact_dead`, `with_template_keys`; `version` is bumped by
`bump_version` and hand-inlined in `insert_into_map_with_tombstones`, and reset by
`rebuild`. The shape rules in the `Table` doc are the contract and nothing checks
them. Fix: `debug_assert_invariants()` (dead_count equals nil-slot count; no live
hash entry for keys `1..=array.len()+1`) at the end of every mutator under
`debug_assertions`, as `drain_mark_worklist` did for `mark`. Lateral:
`with_template_keys`' defensive duplicate count absorbs what would mean a verifier
bug; a `debug_assert_eq!(map.len(), key_ids.len())` would surface it.

## HYG-059 - Snapshot encoder holds a breadcrumb per encoded value

Reported by the heap hunter.

`SaveBuilder::breadcrumbs` pushes a `Breadcrumb` (a `String` for globals) per
key, value, metatable and upvalue, only so `UnregisteredFunction` can render a
path: a second copy of the graph's size for the whole save. `encode_env_deltas`
pushes a fresh root breadcrumb with a newly truncated token copy per key and
value. Fix: record nothing on the happy path; on failure re-walk from roots. Also
`SaveBuilder::push_path` truncates every `PathSegment::Global` to
`MAX_PATH_GLOBAL_BYTES`, and all eight call sites (`finish`, `encode_env_deltas`
x7) already truncate before constructing; delete the call-site copies and make
`PathSegment::global(&str)` the only constructor.

## HYG-064 - `OP_NEW_TABLE_PRESIZED` sizes only the hash part

Reported by the heap hunter (parser side not confirmed).

`frame.rs` passes `A` to `new_table_with_capacity`, which presizes the
`IndexMap`; the array part is never presized. If `A` counts positional items,
`{1, ..., 10}` allocates an unused 10-slot hash map and grows the array by
doubling. Check what the parser emits.

---

## Stdlib and patterns

## HYG-069 - Exact-integer bound is owned by `table.rs`, not `numeral.rs`

Reported by the stdlib hunter; narrowed in wave 1.

The table library now uses one range rule for `table.move` and the sequence
functions, but the 2^53 bound itself is still `table.rs`'s `MAX_EXACT_KEY`, while
`vm/rng.rs` writes `1u64 << 53` for a related purpose and `numeral::exact_i64`
(in `src/numeral.rs`) owns the conversion. Fix: `numeral::MAX_EXACT_INTEGER`
beside `exact_i64`, imported by both.

## HYG-072 - Argument-error construction bypasses its owner; library error text is free strings

Reported by the stdlib hunter.

`vm_aux.rs::check_type` builds typed `ArgError { TypeMismatch, .. }`. Bypasses:
`basic.rs::arg_type_error` (used once by `setmetatable`); `math.min`/`max`'s
hand-built "no arguments" error, which is exactly what `check_type_named(state, 1,
Number, ..)` produces on the next line (dead in effect); `string.rs::
check_string_or_number` claiming `expected: String`; `string_format.rs::format`
using `ArgDetail::Custom` free text where others use `TypeMismatch`, so a host
matching `TypeMismatch` misses `string.format`. Beyond that, one class of event
goes through several channels: "wrong number of arguments" (`math.random`,
`table.insert` x2) as `RuntimeError(String)`; "position out of bounds" etc. via
`ArgDetail::OutOfBounds(String)`; "table or string expected" via `Custom`; gsub's
"invalid capture index" as `RuntimeError` while the pattern's is a typed
`PatternErrorDetail`; about 20 ad hoc literals ("invalid conversion '...' to
'format'" spelled twice in `parse_directive`, "cannot change a protected
metatable", ...). Hosts cannot match without parsing text, and the
reference-pinned wording is scattered. Fix: `check_type_in(state, n, &[LuaType],
name)`; a `LibError` enum (or payload-free `ArgDetail` variants) whose `Display`
owns the wording. Enforceable: private `ArgError` fields/constructors; removing
`Custom`/`OutOfBounds(String)` and making `RuntimeError` non-constructible outside
`error.rs`.

## HYG-073 - Library errors shed context

Reported by the stdlib hunter.

`match_error` / `pattern_runtime_error` produce `PatternError(detail)` with no
function name, so a malformed pattern from `gsub` and from `find` look the same
to a host (reference shares the gap; the typed error could carry it free). gsub's
"invalid capture index" omits the index the code holds. `sort_less`'s comparison
`TypeError` does not name `table.sort`.

## HYG-075 - `Result::unwrap_or` bypasses the `Result::ok` ban

Reported by the stdlib and compiler hunters.

The `Result::ok` ban exists to stop silent discards; `unwrap_or` on a `Result`
does the same. Sites: the gmatch iterators' `to_number(-1).unwrap_or(0.0)`
(HYG-052), `error(msg, level)` saturating via `unwrap_or(u32::MAX)` (harmless),
and `numeral::parse_decimal`'s `Some(parsed.unwrap_or(f64::NAN))` after an
`is_err` check, which exists only to dodge `unwrap_used` and makes a reader prove
NaN cannot reach a literal pool (use `match`). Enforceable: add
`core::result::Result::unwrap_or` and `unwrap_or_default` to `disallowed-methods`
with scoped allows where a default is meant.

## HYG-076 - Stdlib invariant panics restated per site

Reported by the stdlib hunter.

`vm_aux.rs::is_none_or_nil` asserts `arg_number != 0` while public siblings
`check_any`/`check_type` return `InvalidStackIndex` for 0; make it the same.
`isize::try_from(state.get_top()).expect("the value stack limit fits in isize")`
appears three times in `tonumber` and again in `print`, `math.min`/`max`,
`table.pack` (about 8 copies); a `State::top_isize()` would own it.
`string.find`'s `u8::try_from(n - 1).expect(..)` relies on `LUA_MAXMATCHES <= 255`
asserted in `string.rs`; put `const _: () = assert!(..)` beside the constant.
`string_format.rs::format_float`'s `unreachable!` becomes unrepresentable with
`Conversion::Float(FloatKind)`.

---

## Host surface and CLI

## HYG-082 - Cost-analysis caveat and CLI output strings restated at several sites

Reported by the host-surface hunter.

The `--analyze` caveat is an inline `eprintln!` in `main.rs`, prose in the
`analyze_cost` doc, and restated in `docs/cli.md` and `docs/budget.md`, already
worded differently ("not a runtime bound or native-cost estimate" vs "neither a
runtime lower nor upper bound, and it is not a native-cost estimate"). A host
printing a `CostAnalysis` gets no caveat. The `"Error: "` prefix is spelled in
`report_error` and `report_usage_error`; `"Cost used: {}"` inline in `main`, as a
literal in `tests/cli_limits.rs`, and in `docs/cli.md`. The test cannot import from
a bin crate. Fix: `pub const ANALYSIS_CAVEAT` (or `CostAnalysis::caveat()`) and a
`cli` module (or lib behind a feature) owning argument parsing and output formats
that tests import. Enforceable by a CLI test asserting stderr contains the const.

## HYG-083 - Cost-category counters enumerated at four sites

Reported by the host-surface hunter.

`ScopeCost` (7 counters), `ScopeTotals` (same plus `function_count`),
`ScopeCost::accumulate`, `fmt_indent` and `CostAnalysis::fmt` enumerate categories
field by field; only the `OpcodeCost` match in `analyze_chunk` is exhaustive.
Labels re-encode opcode knowledge ("Arithmetic (+,-,*,/,%,^)") owned by
`instr.rs`. Fix: `counts: [u64; OpcodeCost::COUNT]` with labels as an
`OpcodeCost` method. Enforceable by type. Lateral: a zero-cost scope prints
`cost 0 (free)` only when `function_calls == 0`, otherwise `cost 0` plus the
breakdown (intentional per comment); `ScopeTotals.total_cost` is copied from root
only to print.

## HYG-084 - `Error` exposes the same data through pub fields and accessors; `on_error` source duplicates it

Reported by the host-surface hunter.

`Error` has `pub line_num`, `pub column` and `line_num()`/`column()`; `kind` and
`stack_trace` are pub while `context` is private. Pub fields let hosts mutate or
rebuild an error and lose the `reported` flag that gates `on_error` firing once,
whose safety depends on re-raise paths carrying the same value.
`on_error(source: Option<&str>, error)` passes a source that `Error::source_name()`
and the first stack frame also carry; the `Display` doc spends a paragraph on how
splicing two of them once produced wrong output. Fix: private fields with
accessors; drop the parameter or document precisely why it differs. Enforceable by
visibility.

---

## Tooling, tests and harnesses

## HYG-086 - Examples corpus walked by five implementations with different rules

Reported by the tooling and compiler hunters.

`tests/run_examples.rs::collect_lua_files` (cwd-relative, follows symlinked dirs),
`tests/diff_test.rs::collect_examples::visit` (cwd-relative, follows symlinks),
the `src/compiler.rs` unit test `collect_lua_sources` (`CARGO_MANIFEST_DIR`, `> 20`
vacuity floor), `scripts/diff_test.sh::append_lua_files` (`find -type f`, does not
follow symlinks, `LC_ALL=C` sort), and `bench.sh` (reads brokkr.toml).
`run_feature_test_extended_in_process` hard-codes one example path the walker
already covers. Anchoring, vacuity guards and sort order (PathBuf component order
vs C-locale byte order) differ; they agree only because the tree has no symlinks
or diverging names. A second extension or a skip convention would split them. The
compiler unit test cannot reach `tests/common`, so that copy is forced by the
unit/integration boundary unless the test moves. Fix: one
`tests/common::example_corpus()` anchored at `CARGO_MANIFEST_DIR`; a `cfg(test)`
helper in src or move the compiler test; a parity test between the shell and Rust
lists. Enforceable by the parity test.

## HYG-088 - Reference-Lua set and install hint spelled in six places

Reported by the tooling hunter.

"lua5.2 and lua5.4" appears in `diff_test.sh` (tool check loop, two `run_lua`
sites, TIMEOUT labels), `tests/diff_test.rs` (`lua_versions_available` runs each
with `-v`; the assertion carries an `apt install` hint), `diff_gate.sh`'s comment,
a brokkr.toml comment and AGENTS.md. `bench.sh` has a legitimately different set
(`lua5.5 lua5.4 lua5.2 luajit`) with no shared naming. Already drifted: Rust checks
`-v` exits zero, shell checks `command -v`; the hint exists only in Rust. Tools
are assumed under Debian names; on distros shipping `lua52` or `lua-5.4` the result
is a loud refusal. Fix: a `REFERENCE_LUAS` array in lib.sh, optional
`LUA52=`/`LUA54=` overrides; the Rust test should not pre-check. Enforceable by
deleting the second site.

## HYG-089 - Tooling knobs have no inventory; `FEATURES` is generic and cannot be empty

Reported by the tooling hunter.

Env vars: `DELLINGR`, `DELLINGR_SKIP_BUILD`, `DELLINGR_DIFF_TIMEOUT`,
`DELLINGR_SKIP_TIMEOUT`, `DELLINGR_TEST_LIMIT_KB`, `FEATURES`,
`BROKKR_MARKER_FIFO`. Constants: `DEFAULT_ITERATIONS`, `MAX_WARM_BLOCKS`,
hyperfine `--warmup 2` (in both `bench.sh` and `hotbench.sh`), the 5s diff timeout,
the 2 GiB vm cap, bucket directory lists, `FUNCTION_COUNT`, the 30-call footer.
AGENTS.md omits `DELLINGR_SKIP_BUILD` (documented in `diff_test.sh`), so the
documented set already diverges. In `hotbench.sh` an ambient `FEATURES` from CI or
another tool changes which harness is built, and `${FEATURES:-hotpath}` makes "no
features" unrepresentable. Fix: lib.sh resolves all shell knobs with a header
listing them (`HYPERFINE_WARMUP` there); rename to `DELLINGR_HOTPATH_FEATURES` or a
`--alloc` flag.

## HYG-090 - bench/ and examples/ kernel pairing is unchecked

Reported by the tooling hunter; narrowed in wave 1 (`tests/bench_scripts.rs` now
parses every bench/ file and checks `_bench`, the 30-call footer and the success
line).

The pairing invariant (each brokkr.toml `file` and `hotpath_file` are the same
kernel in the same proportions) is still unchecked, and a kernel in examples/
that prints nothing still passes `run_examples`. Fix: compare `analyze_cost`
fingerprints of each pair's `_bench`; require at least one `: true` line per
example that defines `_bench`.

## HYG-092 - Tooling diagnostics swallow the failure reason

Reported by the tooling hunter.

`bench.sh` runs `hyperfine ... >/dev/null 2>&1`; on failure the table shows `FAIL`
and the reason is gone, with no JSON. `diff_test.sh`'s `FAIL: <name>` gives no
diff or hint which reference was closer, and temp outputs die in the EXIT trap
(the terse stdout format is deliberate for brokkr's last-line match). Fix: stderr
to `$results_dir/$run_index.err` printed in the summary; keep
`.scratch/diff-fail/<name>.{ours,52,54}` or write detail to stderr. Harness
`counter_value` saturates to `i64::MAX` and `warm_avg_cost` truncates; a saturated
counter should be a refusal rather than a lie in the results DB.

## HYG-094 - Example runner and stress cap: no bound where stress runs, cap where nothing runs

Reported by the tooling hunter.

`run_examples` has no per-example timeout or memory cap; worker count is
`available_parallelism()` with no override, so core count decides how many stress
examples run at once, concurrently with the diff buckets. `scripts/test_limited.sh`
exists "for stress tests" but its only reference is AGENTS.md's command list, and
`ulimit -v` is a poor cap on allocators that reserve large virtual ranges (and
Linux-specific). Either the cap is unnecessary and the script is dead, or the
policy it encodes is not applied where stress examples run. Fix: wire it into the
stress path or delete it; add an override for constrained CI.

## HYG-097 - Dead or out-of-tree tooling

Reported by the tooling hunter.

- `scripts/notes_drop.py`: zero references in the repo or docs; it edits
  `## <ID> - ` sections of findings documents (the tool behind 7996ccf's final
  drain). These hunt documents use that heading format, so it may be relevant
  again; if kept, document it in AGENTS.md's scripts section.
- `.gitignore` entries `overnight.sh`, `overnight-logs`, `.plans` match nothing any
  tracked tool writes (plans/ is retired), and would hide such files from `git
  status` if they reappeared.
- `research/` is ignored wholesale, yet AGENTS.md names
  `research/bench_luars.sh` as the source of the README's lua-rs column; the
  published numbers come from an untracked script on one host. Track it (or move
  to `scripts/`) and ignore only `research/lua-rs/`.
- `diff_test.sh`'s timeout fallback chain (`timeout` -> `gtimeout` -> a 20-line
  embedded Python reimplementation of GNU duration parsing) is load-bearing only
  on hosts without coreutils, while the project already assumes GNU-ish tools; if
  macOS is not a goal it is dead weight kept in step with `timeout(1)` by hand.

## HYG-098 - Stale and drifting comments and docs

Reported by every hunter, at different sites.

- `examples/hotpath.rs` refers to "hotpath 0.15 ... as of 0.16" while Cargo.toml
  pins 0.28.x; reword without versions. Its `use std::process::exit` comment says
  brokkr is the only caller, contradicting the module doc and AGENTS.md
  (`hotbench.sh` also calls it). Its module doc says `--iterations N, default 20`,
  duplicating `DEFAULT_ITERATIONS`.
- brokkr.toml says `[test] debug = true` while AGENTS.md says `brokkr test` always
  passes `--release`; one is stale or the key means something else. brokkr.toml
  says diff_gate prints `FAIL: <path>`; actual lines are `FAIL: <name>`,
  `FAIL: <name> (dellingr exited N)`, `TIMEOUT: ...`; reword without enumerating.
- `large_source` registers its ms-scale file for `--bench`; the documented
  rationale for bench/ does not hold for its verdict walls (only `parse_us` is
  meaningful) and nothing in the results flow marks that. The harness exits on
  error paths without dropping the hotpath guard (intended), so a late `--alloc`
  failure reports nothing about where it was.
- `table.rs` (compiler) comment cites an example file and DIFF tag for the
  leading-separator divergence; `docs/compatibility.md` is the durable owner.
- `exp_desc.rs` comment cites "the capacity findings", transient-document language
  in a durable comment.
- `parse_pow` comment says "part of the cost model since version 4"; drop the
  drifting specific or point at `COST_MODEL_VERSION`'s history.
- `call_op`'s historical-bug narration ("Previously, using `f(self)?` here caused
  a bug ... segfaults") describes code that no longer exists and claims segfaults
  the current safe code cannot produce; replace with the invariant.
- `verify.rs`'s comment names only the corpus test as the guard while `finalize`
  validates every compile (HYG-001).
- AGENTS.md says the CLI carries both `disallowed_methods` and `disallowed_types`
  allows; `main` carries only the first. Reword to "scoped allows as needed".

## HYG-100 - `Val`'s `Debug` prints a process-local slotmap key for strings

Reported by a wave 1 fixer.

`Val`'s `Debug` (`src/vm/lua_val.rs`) forwards string values to `StringPtr`'s
derived `Debug`, which renders the slotmap key: process-local, so any message or
log built from `{:?}` of a `Val` differs between runs. No production formatting
use was found. Fix: render the string contents (or a fixed placeholder) in
`Val`'s `Debug`.

## HYG-101 - `tests/test_conventions.rs` asserts source spelling, not behaviour

Reported by the wave 1 reconciler.

The conventions scan pins exact substrings of `tests/common/save_fixture.rs`
(the `env::var_os` match arm text), so formatting or a harmless refactor breaks
it. Test the regeneration gate's behaviour instead. Related: `run_examples.rs`
builds its own `Command::new(common::dellingr_bin())` rather than
`tests/common/cli.rs`'s `command()`.

## HYG-102 - Snapshot `FORMAT_VERSION` lives in `instr.rs`

Reported by the wave 1 reconciler.

It moved there so the opcode fixture test can see it in non-snapshot builds, but
the snapshot format version is now owned by the bytecode module. An ungated
home for persistence constants (or `limits.rs`-style module) would fit better.

## HYG-099 - `#[hotpath::measure]` on trivial functions distorts the distribution

Reported by the compiler and VM-core hunters.

`Parser::update_line` (a one-field setter called per token) and
`State::get_string_constant` (a one-line forward) carry measurement points; under
instrumentation they inflate the event queue and mostly measure themselves. Remove
them.
