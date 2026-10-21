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

## HYG-001 - Cache-slot operand encoding has four owners and two sentinel conventions

Reported by the compiler and VM-core hunters.

- `GET_GLOBAL`/`GET_FIELD`/`SET_FIELD` use 255 for "uncached"; the VM relies on
  `caches.x.get(255)` returning `None` because no cache vector reaches 256
  entries, which nothing asserts. `frame.rs` passes a literal `u8::MAX` for
  `SET_FIELD_AT` to mean "no cache".
- `SET_GLOBAL`/`TFOR_CALL` bias by one (0 = uncached), decoded with
  `checked_sub(1)` in `eval_store.rs::instr_set_global` and
  `eval_control.rs::instr_tfor_call_next`. `SET_FIELD_AT` with A=0 means "not yet
  rewritten". All persisted.
- Each rule is spelled in the `instr.rs` constructors, `assign_cache_slots`, the
  verifier (`compiler/verify.rs`) and the VM decode sites. `RuntimeCaches::new`
  recomputes the TFOR cursor count a third way (max C operand) because storing a
  fourth count would change the snapshot format.
- The verifier copy is a deliberate independent check, and `finalize` runs
  `validate_bytecode` on every compile, so drift fails every compile (the
  verify.rs comment undersells this by naming only the corpus test). The VM
  decode copy has no checker.
- Related sentinel copies: `OP_VARARG` checks `n == u8::MAX` for "all values" in
  `frame.rs` and `verify.rs` instead of `RetCount::from_u8`; `ArgCount`/`RetCount`
  panic messages hard-code "below 255" beside `limits::MAX_FIXED_ARGS = u8::MAX -
  1` and `MAX_ARGUMENTS = u8::MAX`.

Fix: typed operand decoders on `Instr` paired with the encoders
(`global_get_slot(self) -> Option<u8>` etc.) used by VM, verifier and finalizer;
at the next format bump unify on one convention (biased, 0 = uncached) and store
the TFOR count beside the others, retiring the recomputation. Format the panic
strings from the const. Enforceable: decoders make raw decodes stand out; a grep
for `== u8::MAX` / `inst.a()` near cache opcodes in `src/vm/`; a verifier test
pinning cache vectors below the sentinel.

## HYG-002 - SET_LIST batch stride: VM spells `u8::MAX`, compiler spells `MAX_TABLE_BATCH_SIZE`

Reported by the VM-core hunter.

`eval_store.rs::instr_set_list` computes the start index as
`batch * u8::MAX + 1`; `compiler/parser/table.rs` flushes at
`limits::MAX_TABLE_BATCH_SIZE`. Both 255 today. Changing the limit silently
writes every multi-batch array constructor to the wrong indices, which is
replay-visible. Fix: read the limit, or an `Instr::set_list_start_index(batch)`
shared by encoder and decoder. Enforceable: a VM test of a 2-batch constructor
reading back `t[256]`, or the method.

## HYG-003 - Snapshot `FORMAT_VERSION` is spelled in the fixture test, cross-checked only under a feature

Reported by the compiler hunter.

`save_state.rs` has `FORMAT_VERSION: u16 = 7`; `instr.rs`'s
`persisted_encodings_match_fixture` asserts the literal `"format=7"`; the fixture
file holds a third copy. The cross-check against the live constant runs only
under `cfg(feature = "snapshot")`, so in a default build fixture and test can
agree with each other and disagree with the writer. Fix: make the constant
`pub(crate)` and compare without the gate. Enforceable by that test.

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

## HYG-014 - `COST_MODEL_VERSION` is not bound to the golden table it guards

Reported by the host-surface hunter.

`tests/cost_golden.rs` detects drift and says to bump the version, but editing
the expected numbers without bumping passes. Fix: a `GOLDEN_VERSION` const in the
same file asserted equal to `COST_MODEL_VERSION`, or a digest of corpus+values
beside the version. Enforceable by test.

## HYG-015 - Constant folding contradicts `docs/budget.md` and leaves dead pool entries

Reported by the compiler hunter.

Folding `PushNum; PushNum; Add` (and `Negate` of a literal) removes a charge, so
`x = 60 * 1000` costs 0, `local k = 60; x = k * 1000` costs 1, and `2 ^ 10` costs
1 (pow never folded). `docs/budget.md` says naming an intermediate must not make
a script more expensive. Only `tests/cost_golden.rs` (`folded_literals`) pins it;
the only doc mention is `docs/determinism.md`'s note on `^`. Either document
folding as a deliberate exception or charge folded expressions (awkward:
per-chunk static "folded units").

Folding also leaves intermediates (`60`, `1000`, `5`, `-5`) in
`number_literals`: they count toward `MAX_LITERALS`, are serialized into
snapshots and become per-State `BytecodeRuntime.literals` slots. Fix: compact in
`finalize` (remap `PUSH_NUM` Bx); changes snapshot contents of identical source,
so needs a note but not a format bump.

---

## Compiler front end

## HYG-016 - Table inline-template threshold spelled twice

Reported by the compiler hunter.

`table.rs` uses `field_count > 4` in `parse_table` and `[TableTemplateField; 4]`
plus `self.inline.len()` in `TableTemplateCandidate`. Changing one without the
other makes `fields_for_template` silently return `None` for 5..N fields. Fix:
`const INLINE_TEMPLATE_FIELDS`. Enforceable by the const, or a test that a
5-field named constructor gets a template.

## HYG-017 - Call/index suffixes charge two syntax levels, a hidden multiplier on `MAX_SYNTAX_DEPTH`

Reported by the compiler hunter.

`expr.rs` calls `enter_syntax_level` twice per call/index suffix (justified by
debug-build frame size), so nested calls get 100 levels and parentheses 200.
Neither `limits.rs` nor `docs/limits.md` says so. Reference `LUAI_MAXCCALLS` is
also per-construct, so it may be acceptable, but it is undocumented. Fix:
`limits::SYNTAX_COST_SUFFIX = 2` (or a `SyntaxCost` enum) and document it; the
`deepest_nested_programs_fit_verifier_marker_limit` test already depends on the
ratio and could read the constant.

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

## HYG-022 - Per-function parser state declared twice and moved field by field; dummy root frame; three depth counters

Reported by the compiler hunter.

- The per-function fields (`chunk`, `literal_indices`, `locals`, `upvalues`,
  `loop_breaks`, `fold_barrier`) are declared on `Parser`, again on `FuncState`,
  listed in `take_func_state` and `restore_func_state`, and initialized in
  `parse_str_named`: five restatements. The `FuncState` doc records the bug this
  already caused (`loop_breaks` outside the unit made a `break` patch the wrong
  chunk). A missed field is filled silently by `Default`.
- `parse_str_named` builds a dummy initial `chunk` to carry `source`; the root
  immediately suspends it into `outer_funcs[0]`, and that off-by-one is what
  makes `outer_funcs.len() >= MAX_CHUNK_NESTING` mean "depth including the root"
  as documented. Root-level upvalue resolution searches the empty dummy every
  time.
- Three depth counters with three types: `nest_level: i32` (never negative),
  `syntax_depth: u32`, `outer_funcs.len()`; the `(name, i32)` local tuples store
  the level again per local.

Fix: a single `funcs: Vec<FuncState>` whose last element is current
(`mem::take`/`mem::replace`), depth = `funcs.len()`, source name a `Parser`
field, block-start index stack instead of per-local levels. Also unifies
HYG-023. Enforceable structurally.

## HYG-023 - Upvalue resolution written twice

Reported by the compiler hunter.

`resolve_upvalue_recursive` and `create_parent_upvalue` (upvalue.rs) implement
the same lookup at two "current" levels (`self.upvalues` vs
`outer_funcs[parent_idx].upvalues`); the capacity check is spelled in
`add_upvalue` and `checked_parent_upvalue_index`. Fix: one `resolve_in(level,
name)` over the uniform stack from HYG-022 (reference `singlevaraux`).

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

## HYG-033 - Verifier and finalize cost scales badly with source size

Reported by the compiler hunter.

`verify_stack_discipline` clones `AbsState` (two marker stacks up to 200 entries)
into every reachable pc: O(code_len x marker depth) memory per chunk, bounded but
large for 16 MiB sources. `finalize` runs `validate_bytecode` per chunk and
recurses, so a source with many nested chunks pays it many times, in release, on
every `load`. Fix: share marker stacks or store them only at join points.

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

## HYG-035 - Thin wrapper layers and a facade with one bool of payload

Reported by the compiler and host-surface hunters.

`Parser::describe_token_type` returns `typ.spelling()`; `parse_fndef`,
`parse_fndef_named`, `parse_fndef_method` forward to `parse_fndef_with_params`;
two layers of `parse_str`/`parse_str_named` (`compiler::` wraps `parser::`).
`parser::parse_str` exists separately only so tests can see pre-`finalize`
output. On the host side, `Engine::compile`/`compile_named` never reference
`self` (Engine's only state is `install_stdlib: bool`); `analyze_cost(source)` and
`Engine::analyze_cost(&program)` are two entries to one thing;
`State::load_string(_named)` is a third compile path. Fix: one
`compile(source, name)` (test helper for pre-finalize output); either drop
`Engine` for `Program::compile` + `State::new`/`raw`, or move real configuration
(budget defaults, RNG seed, callbacks factory) into it.

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

## HYG-038 - GC schedule and string-slot reuse preserved as if they were replay contract

Reported by the heap hunter (and implied by the VM-core hunter's note that
per-site GC timing "is part of the collection behavior").

Comments in `vm.rs` (`sweep_bytecode_caches`: would change string-slot reuse and
future thresholds) and `table_ops.rs` (`metatable_tostring_field`: pre-interning
can change reuse order) treat allocation order and collection timing as
must-not-change. The heap hunter found nothing in scope that makes either
observable (`pairs` order is insertion-based, pointer ids are counters, snapshot
ids are first-encounter order, `ObjectPtr`-keyed maps are only probed). If they
are contract, `docs/determinism.md` should say so with the reason; if not, the
comments freeze code for nothing and block HYG-037. Needs an explicit decision.

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

## HYG-042 - Rust-function call ABI implemented twice and already diverged on error reporting

Reported by the VM-core hunter.

`eval.rs::call_op` (RustFn branch) and `eval_control.rs::instr_tfor_call_rust_fn`
each set `stack_bottom`, save/restore `active_rust_fn`, call, truncate and
restore on error, normalise the return count (pad nil / `rotate_right`), and
restore `stack_bottom`. `call_op` routes errors through `report_call_error`; the
tfor copy returns them raw and relies on the enclosing frame. Lateral: `call_op`
pads with nils when a RustFn reports more results than it pushed (`Ok(3)` after one
push gives `v, nil, nil`), a leniency not listed in `docs/compatibility.md`
(as far as the hunter checked); list it or make it an error. Fix: one
`invoke_rust_fn(f, call_base, nargs)`. Enforceable: afterwards
`active_rust_fn.replace` appears once.

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

## HYG-045 - Cleanup-on-error and panic-safety decided per site

Reported by the VM-core hunter.

Cleanup: `call_op` has 6 `truncate_call_frame` calls, `eval_closure_frame` 3
`cleanup_closure_frame`, `handle_index_metamethod_inner` and `index_string`
truncate temporaries by hand; a guard or `frame_scope` helper would make "every
exit restores" structural (measure frame bloat on the recursive path before
rejecting; a non-closure struct with plain-store drops adds no landing pad).
Panic safety: `eval.rs` says a State unwound through must be discarded, and the
main path has no guard, yet `with_restricted_env` (whose comment says it exists
"so an embedder that catches the panic and reuses the State" works),
`with_rooted_values` and the vararg path of `eval_closure_inner` use
`catch_unwind`/`resume_unwind`. A host reading `with_restricted_env` concludes
reuse is supported. Fix: a poison flag checked at public entry points (one branch
per host call, not per opcode, which answers `eval.rs`'s hot-path objection), or
remove the three guards. Enforceable by a test that panics in a RustFn under
`catch_unwind` and asserts the next `call` refuses.

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

## HYG-049 - Unbounded or super-linear VM growth paths

Reported by the VM-core and heap hunters.

- `bytecode_caches`: each `load_string` adds an `Arc<Bytecode>` entry swept only by
  GC; with `gc_disable_auto()` repeated loads grow without bound. Same for
  `format_pointer_ids` (documented), pruned only in `gc_collect`. Neither appears
  in `docs/limits.md`'s growth list (not verified).
- `open_upvalues` uses sorted `Vec::insert`: bounded by `MAX_STACK_SIZE`, but O(n)
  per insert, so capturing many locals in a deep frame is quadratic, uncharged (a
  performance concern per the budget doc).
- RustFn re-entrancy (a RustFn calling `state.call` on another RustFn) recurses on
  the native stack without touching `call_depth`; `docs/limits.md`'s "active Lua
  calls" is accurate but easy to misread as bounding all calls.
- Upvalue pool `slots`/`colors` never shrink; free-listed, bounded by peak. Noted.

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

## HYG-052 - gmatch iterator state is a schema spelled as string literals in two modules, read leniently

Reported by the heap and stdlib hunters.

- `lua_std/string.rs` defines `GMATCH_ITER_ID`/`GMATCH_EMPTY_ITER_ID`; `verify.rs`
  re-spells them as `"string.gmatch.iter"`/`"string.gmatch.empty_iter"`. The
  state keys `"s"`, `"p"`, `"pos"`, `"len"`, `"lastmatch"`, `"compile_charged"`
  and integer slot `0` are literals at each read/write in `string.rs` (`"pos"`
  five times, `compile_charged` twice) and again as `b"pos"`, `b"len"`, `b"s"`,
  `b"p"` in `verify.rs`. The table is persisted, so a rename breaks loading of
  in-flight iterators, or worse, an id rename leaves `iterator_kind` `None` and
  the closure is skipped as ordinary. The empty-pattern iterator is a second
  schema sharing `pos`.
- Reads are lenient: `gmatch_empty_iter` (twice) and `gmatch_iter` read `pos`/`len`
  with `to_number(-1).unwrap_or(0.0)`, so a missing or non-number field restarts
  at 0 instead of raising, on storage-sourced input. `gmatch_empty_iter` does not
  type-check argument 1.
- Dead compatibility path: `gmatch_iter` seeds slot 0 with `"compile_charged"` for
  snapshots saved before slot 0 existed (fcc1f9e). `FORMAT_VERSION` has since been
  bumped (f8a4704) and loading requires an exact match, and scripts cannot reach
  the table (upvalue of the wrapper, no `debug`), so the branch is unreachable.
- Ordering invariant: `gmatch_subject_matcher_and_cost_meter` depends on
  `memoize_gmatch_pattern` running immediately before with no allocation between,
  guarded by an `InternalError`; a single `with_gmatch_matcher(.., |subject,
  matcher, meter| ..)` would make it structural.
- The `_G` proxy metatable and string metatable are likewise library-installed
  state the VM tracks as env tokens without a schema owner.

Fix: a typed `GmatchState` with read/write helpers (empty iterator a variant), ids
and keys in one module both sides import, or a dedicated heap object instead of a
Lua table the verifier reverse-engineers; propagate missing fields as
`InternalError`; delete the dead branch. Enforceable by structure plus a test that
saves and reloads a live gmatch iterator (existing verifier tests build payloads
with the literals, which is the duplication again).

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

## HYG-055 - Duplicate-key and disjointness validation runs twice on two key representations

Reported by the heap hunter.

The verifier checks duplicate saved keys, deleted/upserted disjointness and
order-contains-upserts via `resolve_key` on `SavedVal`s; `materialize_payload`
repeats every check via `insert_runtime_key` on runtime `Val`s. The second pass is
genuinely needed (two `Fn` ids can resolve to one address under ICF, two env
tokens to one object), but neither site says why, and the reason strings differ
("duplicate deleted environment key" vs "duplicate materialized deleted key").
Keep only the runtime check for env deltas and pointer ids, or document both and
share a helper parameterised by key type.

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

## HYG-058 - Array part never shrinks, so queue-style tables grow with history

Reported by the heap hunter.

A FIFO written the obvious way (`q[tail] = v` / `q[head] = nil`) appends at
`array.len() + 1` while clearing the front, growing the `Vec<Val>` by one slot per
push forever: 16 bytes per historical push, uncharged; `next_live_from` scans the
dead prefix, so every `pairs`/`next` start is O(total pushes) for a flat charge;
`snapshot_entries` emits one `(key, Nil)` per array slot, so save size and
verify/materialize work grow with history. Clearing the first half of a large
array has the same effect on traversal. The no-shrink rule protects `pairs`
assigning nil and cached slot indices; shrinking only when no traversal can be
mid-flight (save time, or GC) preserves both. `#` on trailing nils is
replay-visible, so any change must be called out.

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

## HYG-060 - Small persistence redundancies

Reported by the heap hunter.

- `retype_load_error` restates every `LoadError` variant to change the phantom
  `E`; split into `LoadError::Setup(E)` + `LoadError::Snapshot(SnapshotError)` with
  one `From`.
- `Encoder::enforce_field_limits` is `true` in production and `false` only in the
  `cfg(test)` constructor, so tests encode payloads production refuses and never
  exercise the encoder's own limit checks; build raw bytes for oversized-field
  decoding and drop the flag.
- `Table::clear_and_insert_entries` has one production caller in
  `materialize_payload`, which already falls back to `rebuild_from_snapshot` when
  the array is non-empty; merging deletes it and the `exact_shape` flag.
- `SavePayload::encoded_size_estimate` ignores bytecode size, which dominates
  script-heavy worlds; the buffer still grows.

## HYG-061 - Two hash functions for one string lookup

Reported by the heap hunter.

`det_hash.rs` calls itself the crate's only hash-map type and fixes foldhash's
seed, but `StringPool` hashes with a hand-rolled FxHash (`hash_string`, pinned by a
test, endian-defined) and stores the `u64`s in an `IndexMap<u64, _>` that foldhash
hashes again (`det_hash` says foldhash may change per target). Neither reaches
observable order. Fix: key by content with foldhash, or a no-op `BuildHasher` for
the `u64` map; drop the pin test's stability claim if nothing needs it. Related:
`det_hash` offers no `IndexSet` alias, pushing code to `IndexMap<K, ()>` and
`BTreeMap<_, ()>` sets (`SaveBuilder::reachable_pointer_values`,
`expanded_environment_objects`, `encode_env_deltas`' `live_keys`); add one.

## HYG-062 - RNG: default seed stated twice; `VmRng` is `Copy`

Reported by the heap hunter.

`vm.rs` constructs `VmRng::seed_from_u64(0)` and AGENTS.md/docs state "default
seed 0"; a `VmRng::DEFAULT_SEED` const would make the claim checkable.
`seed_from_u64` and `from_state` are byte-identical constructors differing in name
and a feature gate. `VmRng` is `Copy`, so `let rng = state.rng;` followed by draws
silently forks the replay-critical stream; drop `Copy`/`Clone` and keep
`state()`/`from_state()`. Enforceable by type.

## HYG-063 - Dead heap items

Reported by the heap hunter.

`StringPtr`'s `Display` prints a process-local slotmap key; the hunter found no
use, making it dead and a non-deterministic rendering waiting to be used in a
message. Verify by removal.

## HYG-064 - `OP_NEW_TABLE_PRESIZED` sizes only the hash part

Reported by the heap hunter (parser side not confirmed).

`frame.rs` passes `A` to `new_table_with_capacity`, which presizes the
`IndexMap`; the array part is never presized. If `A` counts positional items,
`{1, ..., 10}` allocates an unused 10-slot hash map and grows the array by
doubling. Check what the parser emits.

---

## Function identity and host builds

## HYG-065 - Rust function identity is by address: an implicit contract across dispatch, snapshots and equality

Reported by the VM-core, heap and stdlib hunters.

- `eval_control.rs` fast-paths `TFORCALL` by comparing against `base_next` and
  `base_ipairs_iter` addresses (re-exported from `lua_std.rs` for that purpose);
  `base_unpack_values`/`table_unpack_values` keep separate callbacks because
  identity is by address; `active_rust_function_name` and snapshot reverse lookup
  are keyed by address.
- `Val::Hash` for `RustFn` hashes the address (ASLR-dependent, layout only; worth a
  comment so nobody "fixes" order with a hash-ordered map). `rust_fn_ids_by_addr`
  is a `BTreeMap<usize, _>` that would iterate in ASLR order; only probed today,
  nothing prevents iteration. `SaveBuilder::bytecode_ids` and `bytecode_caches`
  key by `Arc::as_ptr as usize`, lookup only.
- `register_rust_fn`'s "smallest id wins" for a shared address makes save bytes
  depend on whether the build folded functions (ICF): the same source built with
  and without LTO can write different `Fn` ids. Equality by address is
  script-visible (`f == g`) and differs under ICF; documented in
  `register_rust_fn`, not in `docs/compatibility.md`, `docs/determinism.md` or
  `docs/save-state.md`.

Fix: a `StdFn` tag or registry entry the dispatch matches on, so addresses are a
registry detail; an address-key newtype without `Ord` exposure; refuse to save a
function whose address has more than one registered id; document cross-build
behaviour. Enforceable by type.

## HYG-066 - Process-global state: `NEXT_STATE_ID`, gmatch wrapper `OnceLock`, scratch State in registry checks

Reported by the VM-core and stdlib hunters.

`anchor::NEXT_STATE_ID` is a process-global `AtomicU64`; not replay-visible
(Anchor's Debug is documented non-deterministic), but it makes State creation
order across threads observable through `Anchor`'s `Debug`/`Hash`. Removing `Hash`
from `Anchor`, or a per-State identity without a global, closes it.
`GMATCH_WRAPPER` is a process-wide `OnceLock` compiled lazily on the first gmatch
in the process, whichever State; wrap per State at `open_string`, or drop the
shim (it exists because `RustFunc` cannot carry upvalues).
`check_registry_compatible` builds a scratch `State::empty()` and, per its own
comment, advances the anchor counter as a side effect of a dry run; listing the
stdlib's (id, fn) pairs as data (HYG-007's slices) removes the scratch State.

## HYG-067 - libm transcendental functions and `powf` make replay output host-dependent

Reported by the compiler, stdlib and host-surface hunters.

`math.sin/cos/tan/asin/acos/atan/atan2/exp/log` call `f64` methods lowering to
platform libm (the module doc admits last-ULP differences); `vm/frame.rs`
`OP_POW` calls `f64::powf`. The compiler carefully refuses to fold `^` so
bytecode carries no host result, but runtime `^` still produces host bits. A
replay recorded on glibc x86-64 and replayed on aarch64 or musl can diverge once a
script branches on such a result. `numeral.rs`'s `2_f64.powi` is exact for powers
of two but still an intrinsic. The clippy ban list covers clock, env and RNG, not
this. Fix: a fixed pure-Rust implementation (e.g. `libm` crate software paths or
core-math ports) owned by `numeral.rs`/`math.rs`, plus `disallowed-methods` for
`f64::sin`, `powf`, `powi`, `exp` and kin; or `docs/determinism.md` states plainly
that cross-platform replay is not guaranteed for these. Enforceable by clippy.

## HYG-068 - Native stack size is an implicit configuration value

Reported by the compiler, VM-core and host-surface hunters.

`MAX_CALL_DEPTH` plus `MAX_METAMETHOD_DEPTH` assume enough native stack; on a
small-stack embedder thread deep recursion aborts instead of returning
`CallDepthExceeded`. Documented in `docs/limits.md` but undetected (no
stack-remaining probe such as `stacker`/`psm`, no per-State override). The
`eval.rs` rules (no `catch_unwind`, no hotpath on the recursive path) show
per-frame bytes are already load-bearing. `call_depth_exceeded_error` enforces it
only at the test thread's stack size. The compiler test
`deepest_nested_programs_fit_verifier_marker_limit` spawns an 8 MiB thread sized
for today's debug frames; bigger frames fail as an abort, not an assertion (same
class as `syntax_depth_headroom`); derive the size from a named constant. Several
`vm/tests.rs` tests push `MAX_STACK_SIZE` values (~16 MB per test thread), worth
knowing under `test_limited.sh`'s cap.

---

## Stdlib and patterns

## HYG-069 - Exact-integer key bound spelled locally; `table.move` and the others already disagree

Reported by the stdlib hunter.

`table.rs` defines `MAX_EXACT_KEY = 1 << 53`; `vm/rng.rs` writes `1u64 << 53` for
a related purpose; `numeral::exact_i64` owns the conversion but not the bound.
`table.move` checks inline with its own message; `check_sequence_keys` formats
`"{function} range exceeds ..."` and its singleton rule (`exact_i64(first as
f64) == Some(first)`) differs from move's range test. Already diverged: move
rejects a singleton key above 2^53 that is still exact (e.g. 2^54) while
insert/remove/unpack/concat accept it. Fix: `numeral::MAX_EXACT_INTEGER` and one
`check_integer_key_range(first, last)`. Enforceable: a test that
`table.move(t, 2^54, 2^54, 1)` agrees with `table.unpack(t, 2^54, 2^54)`.

## HYG-070 - Two heap sorts that must order equal inputs identically, kept in step by hand

Reported by the stdlib hunter.

`vm/table_ops.rs::table_sort` (raw) and `table.rs::sort_through_metamethods`
each contain the heap sort; the latter's comment says equal inputs order
identically, which is replay-visible (equal-key order, comparator call sequence).
Default comparators are duplicated (`table_sort_less` vs `sort_less`, each
building `TypeError::Comparison`). Only the metamethod path rejects `len >=
i32::MAX` ("array too big", spelled twice), reachable via a huge `__len`; the raw
path has no such rule. Fix: one generic heap sort over an accessor trait with
`Vec<Val>` and scratch-table backends; one length rule. Enforceable structurally,
or by a differential test sorting with and without an identity `__index` and
comparing comparator call sequences.

## HYG-071 - gsub replacement grammar implemented twice; keep-original rule duplicated

Reported by the stdlib hunter.

`append_string_replacement` (first match, per byte) and
`compile_replacement_template` + `append_templated_replacement` (later matches)
each implement `%%`, `%0`..`%9`, the single-capture `%1` = `%0` rule and the
invalid-index error; "invalid use of '%' in replacement string" is spelled twice
in the first. The split is justified (same error point, same charge) but nothing
checks equivalence, unlike `find_subslice`'s naive twin test. The Table and
Function arms of `append_gsub_replacement` each implement "nil/false keeps the
original, string/number appends, else error" with the same message. Fix: compile
once into a template that can hold a lazy error position (the `Item::Fail` trick);
one keep-original helper. Enforceable structurally or by a twin test.

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

## HYG-074 - Anchor and empty-pattern handling decided in several places; gmatch rewrite charges an extra unit

Reported by the stdlib hunter.

`luapat::Compiler::compile` decides `anchored` from the first byte; `gsub`
decides again from `pattern.first() == Some(&b'^')`; `gmatch` rewrites a leading
`^` to `%^` by byte surgery to reuse the compiler. The rewrite makes
`charge_bytes` see `len + 1`, one extra replay-visible cost unit per iterator from
an implementation detail. Empty-pattern fast paths in find, match, gsub and
gmatch are four special cases with their own return shapes. Fix:
`CompiledPattern::is_anchored()`; an `AnchorMode::{Honour, Literal}` compiler
flag for gmatch (removes the rewrite and the extra unit, a cost change to call
out); decide once whether the empty fast paths are needed (`str_match` already
handles an empty item list).

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

## HYG-077 - Small stdlib duplications and unreachable checks

Reported by the stdlib hunter.

- `tostring` and `string.format("%s")` each implement "a string bypasses
  `__tostring` unless the shared string metatable has one"; `error` implements the
  opposite (always bypass). One `State::tostring_bytes(idx)` should own the first.
- Lua-index normalisation: `string.rs` has `lua_start_index`/`lua_end_index`,
  `table.rs` has per-function rules (matching 5.4's quirks), `select` its own.
  The rules really differ, but nothing documents them in one place.
- `_G.__index`/`__newindex` handlers check args 2 and 3 via `check_any_named`;
  the VM always passes them and scripts cannot reach the handlers (`__metatable =
  false`), so the checks are unreachable from Lua (a host could call them). Noted
  so nobody cites them as precedent.
- `LuaPattern::new` is uncached for find, match and gsub; a script calling
  `s:find(p)` in a loop recompiles every time (uncharged, bounded). Sharing the
  gmatch cache would be a win without cost-model change.

## HYG-078 - ASCII character classes diverge from locale-following reference

Reported by the stdlib hunter.

`match_class` and `is_lua_whitespace` are fixed ASCII, so `%a` etc. are the same on
every host: a deliberate and correct divergence from reference (which follows
`setlocale`), worth listing in `docs/compatibility.md` if not already. The diff
gate only agrees with reference under a locale where `isalpha` is ASCII-only, so a
host with a Latin-1 locale could produce spurious diff failures on bytes
0x80..0xFF.

---

## Host surface and CLI

## HYG-079 - Host boundary assumes UTF-8: lossy `print`, UTF-8-only source

Reported by the stdlib and host-surface hunters.

`print` converts to `&str` lossily because `HostCallbacks::print` takes `&str`;
scripts can print arbitrary bytes and the host sees U+FFFD, and the 3x expansion
preflight in `print` and `error` exists only because of it. `load_string(&str)` is
the only load API, so the CLI's `fs::read_to_string` refuses non-UTF-8 sources
(a clean refusal naming the path) although Lua strings are bytes; non-UTF-8 paths
become the chunk name via `to_string_lossy`, silently degrading the identifier.
Fix pre-1.0: byte-exact `on_print(&[u8])` and a `load_bytes`.

## HYG-080 - `on_print`'s default body writes to stdout from the library

Reported by the host-surface hunter; the compiler and VM-core hunters noted the
same default as the channel the debug features bypass.

`HostCallbacks::on_print`'s default is `println!`, so a host implementing only
`on_error` inherits stdout writes, and a game host that forgets the override
prints to the game's stdout. The comment defends it as avoiding breaking existing
impls, which is pre-1.0 API preservation. Fix: make `on_print` required; move the
`println!` into `DefaultCallbacks` (currently an empty struct whose behaviour is
all trait defaults) or a CLI-owned callbacks type. Enforceable by signature. EPIPE
behaviour is BUG-006 in `notes/bugs.md`.

## HYG-081 - `debug_parser`, `debug_vm` and `debug_gc` print straight to stdout

Reported by the compiler, VM-core and heap hunters.

`parser.rs` `parse_chunk` (`println!("Compiled chunk: {tmp_chunk:#?}")`),
`vm/frame.rs` (`println!("{inst:?}")`), `vm/object.rs::collect` print to stdout
under their features, bypassing `HostCallbacks` and interleaving with
`DefaultCallbacks::on_print`; an embedder's capture sees neither. The `debug_vm`
line omits ip, frame and source; `debug_gc` reports object counts only (no
strings, threshold, freed). Nothing in brokkr.toml, scripts or tests enables
`debug_vm`. Fix: one `trace!` implementation to stderr or a
`HostCallbacks::on_trace`/`debug` hook with ip and `bytecode.name`; or drop them
for hotpath instrumentation and a `--dump-bytecode` CLI flag. Enforceable:
`clippy::print_stdout`/`print_stderr` denied in the library, allowed in `main.rs`,
`examples/hotpath.rs`, the trace macro and the default print.

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

## HYG-085 - Public error module exposes internal construction; `is_recoverable` has no consumer

Reported by the host-surface hunter.

`error::results_count` (an ABI validator) and `ErrorKind::argument_detail` are
`pub`; `Error::new`/`without_location`/`with_stack_trace` let a host fabricate a
stack trace. Some is needed by `RustFunc` authors; audit and make the rest
`pub(crate)` (`unreachable_pub` does not help since `error` is public).
`Error`/`ErrorKind`/`SyntaxError::is_recoverable` exist for a REPL that does not
exist; only tests call them. Keep with a doc saying it is for host REPLs, or drop.
Docs drift: `lib.rs` crate docs say callbacks "handle errors"; `on_error` is
notification only.

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

## HYG-087 - The differential gate is implemented twice, with different timeout policy

Reported by the tooling hunter.

`scripts/diff_gate.sh` (a `[[script_check]]`, debug build, 5s timeouts) and
`tests/diff_test.rs` (three buckets shelling out to `diff_test.sh` with
`DELLINGR_SKIP_TIMEOUT=1`, test-profile binary) are the same gate; `brokkr check`
runs tests once per sweep (default, snapshot), so the corpus is diffed at least
three times per check. The Rust copy disables timeouts, so one hanging example
hangs `cargo test` unbounded; its one addition is the snapshot-feature build.
`KERNEL_DIRS_A`/`_B` are tuned to "the gate's per-test time budget", a brokkr
tunable in another repository nothing here names; a new kernel directory silently
lands in the "top-level" bucket. `DELLINGR_SKIP_TIMEOUT` is called "legacy" in
`diff_test.sh` with `DELLINGR_DIFF_TIMEOUT=0` as replacement, yet the Rust test is
its active user and AGENTS.md documents it as current; SKIP also skips the
timeout-tool presence check. `tests/diff_test.rs` runs the script directly (needs
the exec bit) while brokkr.toml runs `bash scripts/diff_gate.sh`. Recommendation
from the hunter: delete `tests/diff_test.rs` and have `diff_gate.sh` take the
feature set from brokkr, or delete the script_check and give the Rust test a
timeout; delete SKIP_TIMEOUT and make `DIFF_TIMEOUT=0` skip the tool check; one
invocation style (`bash <script>`).

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

## HYG-090 - bench/ files are never executed outside a verdict run; footer conventions unchecked

Reported by the tooling hunter.

`bench/` is outside every examples/ glob, so no test parses or runs those files;
a port that stops compiling after a language change (e.g. 0.5.0's newline-paren
rule) surfaces only minutes into a `--bench` run. The pairing invariant (same
kernel, same proportions) is explicitly unchecked (brokkr.toml comment). Every
kernel hand-spells `for i = 1, 30 do _bench() end` and `print("<name>: true")`;
the harness hard-codes `_bench`, `run_examples` hard-codes `": false"`. Nothing
checks a bench/ footer is 30 (it shifts SETUP), and a kernel that prints nothing
passes `run_examples`. Fix: a test parsing every `bench/*.lua` and asserting it
defines `_bench`; per-kernel assertions on the footer line and exactly one `: true`
line; `_bench` as a const shared by harness and test; optionally compare
`analyze_cost` fingerprints of each pair's kernel. Enforceable by test.

## HYG-091 - Benchmark tooling: three naming schemes, a hand-rolled TOML parser, unpruned results

Reported by the tooling hunter.

brokkr addresses workloads by registry name, `hotbench.sh` by `category/name`
(building `examples/${TARGET}.lua`), `bench.sh` by path; nothing maps one to
another. `bench.sh` parses brokkr.toml line by line, breaking on single quotes,
inline comments or a `hotpath_file` key in another table; Python 3.11 ships
`tomllib`, or brokkr could list workloads. `bench.sh` writes
`.scratch/bench-results/<run_id>/` every run and never prunes; nothing reads old
runs, and brokkr's results DB is the owner of benchmark history. Fix: `hotbench.sh`
takes a workload name and resolves `hotpath_file` (enforcing "never instrument a
bench/ file" by construction rather than by the accidental `examples/` prefix);
`tomllib`; keep the last N runs or declare the directory disposable.

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

## HYG-093 - Test helpers duplicated; `tests/common` bypassed

Reported by the tooling hunter.

`tests/cli_arguments.rs` and `tests/cli_limits.rs` each define `ScratchScript`
with near-identical Drop logic and call `env!("CARGO_BIN_EXE_dellingr")` directly,
although `tests/common::dellingr_bin` exists to own that ("Tests must use this").
Only `run_examples.rs` and `diff_test.rs` use common. Fix: move
`ScratchScript`/`ScratchDirectory` and `run` into `tests/common`. Enforceable by a
text check that `CARGO_BIN_EXE_` appears only in `tests/common/mod.rs`.

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

## HYG-095 - Generator restates compiler limits

Reported by the tooling hunter.

`examples/parse/generate.py` has `MAX_NESTED_FUNCTIONS = (1 << 8) - 1`
duplicating `limits::MAX_NESTED`; its docstring cites reference Lua's 200-local
limit while dellingr's `MAX_LOCALS` is 255, and says "parser/func.rs stores the
count in u8" when the limit lives in `limits.rs`. The duplication is forced
(Python cannot import Rust); `run_examples` failing to parse an over-limit file is
what keeps them in step. Fix: cite `limits.rs`, or have `--check` read the
constant from it.

## HYG-096 - Exit-code vocabulary is an unstated cross-file assumption

Reported by the tooling hunter.

CLI always exits 1 (documented in `main.rs`; see BUG-006 for 101); the harness
uses 1 and 2; scripts use 1/2 for usage, 124 for timeout, 125 for a bad timeout;
the Python fallback maps signals to `128 - returncode`. `diff_test.sh` treats
dellingr status 124 as TIMEOUT, which holds only because the CLI never exits 124.
A comment in `diff_test.sh` citing `main.rs`'s exit policy would make it explicit.

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

## HYG-099 - `#[hotpath::measure]` on trivial functions distorts the distribution

Reported by the compiler and VM-core hunters.

`Parser::update_line` (a one-field setter called per token) and
`State::get_string_constant` (a one-line forward) carry measurement points; under
instrumentation they inflate the event queue and mostly measure themselves. Remove
them.
