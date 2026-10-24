# Verification hunt: compiler-cost

Scope: the compiler's in-tree tests (`src/compiler/parser/tests.rs`,
`src/compiler/parser/line_attribution_tests.rs`, the test modules of
`src/compiler.rs`, `src/compiler/lexer.rs`, `src/compiler/codegen.rs`,
`src/compiler/verify.rs`), `tests/lexer_diagnostics.rs`, `compiler_limits.rs`,
`name_limits.rs`, `string_literal_errors.rs`, `count_encoding.rs`,
`bytecode_runtime.rs`, `tests/fixtures/opcode_encoding.txt`, the cost tests
(`cost_golden.rs`, `cost_determinism.rs`, `cost_check_interval.rs`,
`stdlib_cost_golden.rs`, `string_cost.rs`, `call_counts.rs`,
`reentrant_opcode_accounting.rs`, `host_concat_count.rs`) and the code they pin
(`src/cost_meter.rs`, `analyze_cost` in `src/lib.rs`, `src/instr.rs`,
`src/disassemble.rs`), plus the parts of `src/vm/frame.rs` the cost tests ride
on.

Nothing here was run; every finding comes from reading. Findings are grouped by
the five questions, not ranked. Each one says whether the fixed version can be
held mechanically and by what.

---

## 1. Tests that prove nothing

### 1.1 `direct_and_native_meters_share_budget_semantics` compares a function with itself

`src/cost_meter.rs` test module. It asserts that `State::consume_cost` and
`State::cost_meter().consume` agree on acceptance, `cost_used` and
`cost_remaining`. `consume_cost` is `check_poisoned()` followed by
`charge_cost`, and `charge_cost` is `self.cost_meter().consume(cost)` (in
`src/vm.rs`). Both sides are the same call, so the equality cannot fail. The
only independent bit is the trailing `cost_used() == u64::MAX` saturation check
in count-only mode. The real independent oracle for the meter is
`consume_units_matches_the_unit_at_a_time_loop_exactly`, which is sound.

Fix: delete the equality half, keep the saturation assertion. Enforcement: none
needed once deleted; a rule against "assert two wrappers of one function agree"
is not mechanically expressible.

### 1.2 `name_limits.rs`: the snapshot block compares two saves with nothing between them

`source_names_accept_the_limit_and_reject_one_more_byte`, under
`cfg(feature = "snapshot")`, takes `before = state.save_state()` and then
immediately asserts `state.save_state() == before`, with the message "rejected
load leaves state savable". The rejected operation (the oversized-name
`Program::compile`) happened earlier and never touches a State at all:
`Program::compile` has no State to disturb. The assertion only re-checks that
`save_state` is deterministic for an unchanged State, which `save_state.rs`
covers. The file's doc comment ("rejected ... without disturbing the State")
promises a property this test has no way to violate. In the default sweep the
block is compiled out entirely.

Fix: drop the block, and drop the "without disturbing the State" claim for
source names (the compile API cannot touch a State). Not mechanically
enforceable.

### 1.3 Leftover `.and_then(|program| State::new().load(&program))` halves

`compiler_limits.rs` (`expect_syntax_error`,
`set_field_sites_past_the_cache_capacity_use_the_uncached_path`) and
`string_literal_errors.rs` (`malformed_escapes_return_syntax_errors_through_state_loading`)
chain a load into a fresh State after a compile they expect to fail, or compile
the same source twice, once as `dellingr::Program::compile(...).and_then(load)`
and once as `Program::compile(...)` with the message "Program::compile should
accept ...". Those are the same function. This reads like residue of the
migration from a `load_string`-style API to `Program::compile`: the tests still
look as if they cover two entry points, and the test name
"...through_state_loading" claims a path that is never reached because the
compile fails first.

Fix: collapse to one `Program::compile` call per case and rename. Enforcement:
a textlint rule in `brokkr.toml` over `tests/**` banning
`State::new().load(` (a load into a State nobody keeps) would catch this shape.

### 1.4 `string_literal_errors.rs` is a weaker copy of two unit tests

Its six malformed-escape sources are the same six as
`literal_string_escape_errors_use_the_backslash_position` in
`src/compiler/parser/tests.rs` (which also pins the column), and its three
long-string sources are the same three as
`long_strings_return_an_error_at_the_opening_bracket` in `lexer.rs` (which also
pins line and column). The integration copy proves only that the public
`Program::compile` surfaces the same kind, which every other integration test
already proves. It costs one integration binary (one link step per sweep, two
sweeps).

Fix: delete the file, or keep exactly one public-API smoke case in a merged
compiler integration binary (see 3.1). Not mechanically enforceable as such;
merging binaries makes the duplication visible.

### 1.5 `count_only_costs_are_deterministic_across_runs` cannot fail for any reason it names

`string_cost.rs`. Runs one gsub source twice in two fresh States in the same
process and compares `cost_used`. There is no process-wide state on the gsub
path (the only process-global in the library is `GMATCH_WRAPPER`), so this
can only fail on something like uninitialised memory. Determinism that matters
is cross-build, cross-feature, cross-profile and cross-host (see 2.6, 5.1);
none of those is exercised here.

Fix: delete, or replace with the corpus-wide cost golden in 2.6.

### 1.6 `table_concat_cost_is_identical_with_or_without_a_budget` discards both outcomes

`string_cost.rs`. Each run is `match call { Ok(_) | Err(_) => {} }` and only
the two `cost_used` values are compared. The comment says "Some cases in this
list fail on an invalid element on purpose", but nothing checks which case
fails or where. If the first case started failing at its first instruction in
both modes, both report the same cost and the test passes. The equality is
meaningful only together with an outcome check.

Fix: assert the outcome per case (Ok for the first, a `ArgError`/`TypeError`
for the second) and pin its cost. Not mechanically enforceable in general; a
clippy-level ban on `Ok(_) | Err(_) => {}` in tests could be expressed as a
textlint pattern.

### 1.7 Verifier negative tests do not check the reason

`src/compiler/verify.rs` `tree_tests`: `registers_must_lie_inside_the_frame`,
`open_results_need_an_adjacent_consumer_no_jump_enters` (all four negatives),
`cache_families_cap_below_the_encodable_limit` (aliased and miscounted
variants) and `a_cache_slot_owns_one_site` use
`assert!(validate_bytecode(&x).is_err())`. A malformed chunk can be rejected for
a defect other than the one the test constructs. `a_cache_slot_owns_one_site`
is the clearest: it changes the second site's slot to 0 and the declared count
to 1, so the rejection may be the count/order rule, not ownership, which is what
the test is named for. `malformed_sizes_report_the_actual_defect` in the same
module shows the right shape (`expect_err(..).reason == ...`).

Fix: a helper `assert_rejects(&bc, ValidationReason::X)` and convert every
negative. Enforcement: a textlint rule over `src/compiler/verify.rs` test code
banning `validate_bytecode(` followed by `.is_err()`.

### 1.8 `runtime_cache_population_and_hits_charge_nothing` never runs warm across executions

`bytecode_runtime.rs`. The doc says the second execution "re-parses (a fresh
Bytecode identity) while the State already carries warm entries for identical
code". Runtime entries are keyed by `Arc::as_ptr(bytecode)`
(`State::resolve_bytecode_runtime`), so a re-parse gets a brand-new, cold
runtime; nothing is warm except interned strings. The only warm cache hit in
the test is inside the first run (`f(b)` after `f(a)`). The claim "identical on
a cold first execution and a re-parsed second execution" is true but it is
cold-versus-cold.

Fix: also `load` the same `Program` a second time, which reuses the runtime and
actually hits warm caches. Not mechanically enforceable; see 2.9 for the
corpus-wide version of this property.

### 1.9 `gmatch_resume_cost_ignores_fifo_cache_eviction` and friends cannot see whether they evicted

`cost_determinism.rs`. The eviction test assumes 256 distinct patterns evict
the iterator's pattern. 256 is `PATTERN_CACHE_ENTRIES` hard-coded (see 2.4),
and the test has no way to observe that an eviction happened: the compilation
counter `pattern_compilations` is `#[cfg(test)]` on `State`, visible to unit
tests only. The same blindness applies to
`gmatch_first_use_cost_ignores_a_shared_pattern_cache_hit` (no proof of a hit)
and `refusal_leaves_snapshot_state_quiescent` in `string_cost.rs` ("matcher
refusal occurs after the cache hit", unverified). `string_cost.rs` even says
twice "The compile-once half of this check is a crate unit test, since the
compilation counter is test-only instrumentation": the property is split across
two binaries because of where the counter lives.

Fix: expose cache residency and compilation counts to integration tests through
a test-infrastructure feature (the repo already has `fault-injection` for this
kind of hook) or a `#[doc(hidden)]` diagnostic accessor, and assert the
precondition in each test. Enforcement: the precondition assertion is the
enforcement; nothing generic.

### 1.10 `syntax_depth_headroom` runs on whatever stack libtest gives it

`src/compiler/parser/tests.rs`. Its comment says it is "the empirical check
that MAX_SYNTAX_DEPTH fits the stack a debug cargo test thread gets (2MB by
default)". It does not spawn its own thread, unlike every other depth test in
the module, which use `DEEP_PARSE_TEST_STACK_BYTES` precisely "so
`RUST_MIN_STACK` cannot loosen it" (parser.rs comment). Under
`RUST_MIN_STACK=8388608` it proves nothing. It also hard-codes depth 190, paren
95 and `depth / (2 + SYNTAX_SUFFIX_COST)`, not derived from `MAX_SYNTAX_DEPTH`:
raise the budget and it keeps testing 190. And it is superseded by
`nested_statement_and_expression_shapes_compile_within_the_test_stack` and
`deepest_nested_programs_compile_within_the_test_stack` in `src/compiler.rs`,
which measure the exact deepest accepted program on an explicit 2 MiB thread.

Fix: delete. Enforcement: a textlint rule on `src/**` test code requiring that
any test calling `parse_unfinalized`/`compile` with a `.repeat(` nesting
argument runs inside `stack_size(` is too fuzzy; better to route all deep-parse
tests through one helper (`on_parse_test_stack`, see 4.2) and make the helper
the only place a depth loop lives.

### 1.11 Weak or vacuous single assertions

- `compare_branches_keep_comparison_semantics` (parser tests) checks
  `state.call(..).is_err()` for `1 < 'x'`: any error passes. Assert the
  `TypeError`/comparison kind.
- `zero_static_cost_generic_for_is_not_labeled_free` (`src/lib.rs`) asserts
  `!rendered.contains("(free)")`. No code path emits "(free)" any more, so the
  negative half cannot fail unless the string is reintroduced verbatim. Harmless
  as a tripwire; it is not coverage.
- `gmatch_full_scan_is_linear_in_its_subject` (`string_cost.rs`) accepts
  `long <= 3 * short` for a doubling. A quadratic term small next to the fixed
  per-iteration charges passes. The per-word charge is deterministic; pin the
  exact difference `cost(256) - cost(128)` instead.
- `lparen_on_a_new_line_compiles_as_a_call` (`lexer_diagnostics.rs`) only
  checks that it compiles; whether it compiled as a call is what
  `a_new_line_paren_continues_the_call` in parser tests checks by code
  equality. The integration copy adds nothing.
- `decimal_and_hex_literals_use_the_shared_numeral_parser` compares the
  parser's pooled literal with `crate::numeral::parse_lua_numeral`, the
  function the lexer path calls. It is a wiring test (it fails only if the
  lexer stops using the shared parser), not a value test. Fine as wiring; it
  should not be counted as numeral coverage.

### 1.12 `test_lexer01` .. `test_lexer12`

`lexer.rs` unit tests with sequence-number names. A failure report says
nothing about what broke. Rename by behaviour. Not enforceable beyond a
textlint ban on `fn test_[a-z]*[0-9]+\(` in `src/**`.

### 1.13 A test that pins a known-inconsistent message as contract

`lexer_error_positions_stay_pinned_across_newlines` in `lexer.rs` calls its
own expectation a "Pre-existing quirk, preserved": an unterminated short string
ending in `\r` or `\n` reports line 2 column 1, while one ending in `\r\n` or
`\n\r` reports line 1 column 4. That is a position that depends on which
newline convention the author's editor uses, which by AGENTS.md's own rule is a
script-author defect (a misleading message), not a reference-Lua difference to
preserve. The test freezes it.

Fix: make the error position independent of the newline flavour and update the
pin. The fixed version is held by the same test.

---

## 2. Guards and claims that have stopped holding

### 2.1 The dispatch loop's charges are not tied to the opcode cost table

`src/instr.rs` declares each opcode's cost class once (`opcode_rows!`), and
`static_units()`/`fixed_units()` derive from it. `analyze_cost` uses
`static_units()`. `docs/budget.md`'s table is checked against `fixed_units` by
`documented_opcode_charges_match_static_units` in `src/vm/frame.rs`. But the
runtime charge in `Frame::run_frame` is written arm by arm: the six costed
table/negation arms use `add_cost!(const { Instr::fixed_units(OP) })`, the
eighteen arithmetic arms use a literal `add_cost!(1)` inside `arith!`, and the
`Free`/`Call` arms charge by omission. Reclassify `OP_CONCAT` or `OP_LEN` in
the table, or change `Arithmetic`'s units, and `analyze_cost`, the docs test and
the opcode fixture all follow while the runtime does not. Nothing checks
"runtime charge per executed instruction == `static_units()`". The cost goldens
would catch only the opcodes their sources happen to execute.

So the docs claim "These rows describe the units returned by
`Instr::static_units()`" is enforced; the stronger claim implied by
`analyze_cost`'s documentation (that the static walk counts what execution
charges, once per instruction) is not.

Fix, in order of strength:
- Make the dispatch charge come from the table by construction: charge
  `const { Instr::fixed_units(OP) }` in every arm (including arithmetic and the
  free arms, where it folds to zero), or generate the dispatch arm skeleton from
  `opcode_rows!` so an arm cannot exist without its charge.
- Add an execution-count oracle: a test-infrastructure feature that counts
  executed instructions per opcode, and a corpus test asserting
  `sum(static_units * executed) + native_charges == cost_used` for every
  example (native charges via a parallel counter in `consume_cost`).
- At minimum, a unit test in `frame.rs` that, for every known opcode with a
  fixed class, assembles a minimal verified chunk executing it once and asserts
  the `cost_used` delta equals `fixed_units`.

The first is a structural rule; the second and third are tests.

### 2.2 `COST_MODEL_VERSION` discipline covers 18 sources, and does not force a bump

`docs/budget.md`: "It moves whenever `cost_used` for identical source changes."
`src/cost_meter.rs` points at `tests/cost_golden.rs` as the check. What
`cost_golden.rs` actually enforces:

- `cost_used_matches_golden_corpus` and the error corpus pin 18 sources.
- `golden_corpus_fingerprint_matches_cost_model_version` hashes those sources,
  their expected costs and the version. Editing an expected cost forces a
  fingerprint edit in the same file, and the failure message asks the editor to
  bump the version. Nothing requires the bump: update the cost and the
  fingerprint, leave the version, and every test passes. The fingerprint is a
  speed bump whose two sides live in one file.
- Every other cost pin in the repository is outside the fingerprint:
  `stdlib_cost_golden.rs` (every math/string/table flat charge), `string_cost.rs`
  (`basic_native_charges_are_exact`, gmatch caret 26, capture delta 600),
  the other tests in `cost_golden.rs` itself
  (`table_library_native_charges_are_one_or_zero`,
  `unpack_charges_nothing_...`, `mid_run_budget_reset_...`),
  `bytecode_runtime.rs` (12), `reentrant_opcode_accounting.rs`, `lib.rs`
  analysis tests, and `cost_used` assertions in about a dozen more test files
  (`plain_field_miss`, `table_iteration`, `table_metamethods`,
  `global_write_cache`, `error_boundary`, `panic_poison`, `table_sort_metamethods`,
  `table_resolution`, `save_state`, `error_handling`, ...). Changing
  `math.floor`'s charge means editing `stdlib_cost_golden.rs`; no test asks for
  a version bump.

Fix: one cost golden as data, not code. A fixture such as
`tests/fixtures/cost_golden.txt` with `cost_model=<N>` on its first line (like
`opcode_encoding.txt`'s `format=`), one row per pinned source, and every cost
pin in the scope above folded into it. Then make the bump mechanical:
- the test fails unless the fixture's `cost_model=` equals `COST_MODEL_VERSION`;
- the fixture also carries `fingerprint=<hash of rows>`, and the test keeps a
  `const COST_MODEL_HISTORY: &[(u16, u64)]` of (version, fingerprint) pairs,
  failing if the current fingerprint appears under any other version or if the
  current version's recorded fingerprint differs. A changed table can then only
  pass by appending a new (version, fingerprint) pair, which requires a new
  version number. (An in-place edit of the last pair is still possible; a
  `brokkr.toml` script check comparing the pair against `git show HEAD:` would
  close that, at the cost of a git dependency in the gate.)

### 2.3 The native-charge lists are hand-mirrored in three places with no completeness check

`docs/budget.md` lists every math function's flat 1, every string and table
function's rule, and the "Free by design" base functions.
`stdlib_cost_golden.rs` mirrors the math list by hand. Nothing iterates the
library tables. The registrations are declarative arrays
(`lua_std/math.rs`, `string.rs`, `table.rs`; `basic.rs` via `add!`), so adding
`math.tanh` or `table.find` with any charge passes every test. The base-library
"free" list (`type`, `tonumber`, `tostring`, `print`, `select`, `next`, `pairs`,
`ipairs`, `getmetatable`, `setmetatable`, `rawget`, `rawequal`, `rawlen`,
`error`) has no cost pin at all; only `rawset`, `unpack` and `string.len` are
pinned. A free function that starts charging changes replays and is caught by
nothing but incidental goldens.

Fix: a completeness test that walks `_G`, `math`, `string` and `table` with
`pairs` from a fresh State and requires every function value to have a row in
the cost fixture of 2.2 (a free function's row is 0). Optionally check the
`docs/budget.md` lists against the same fixture, the way
`documented_opcode_charges_match_static_units` reads the opcode table. This is a
test; it holds mechanically.

### 2.4 Boundary tests keyed on hard-coded copies of tunable constants

Each of these silently stops testing its boundary when the constant moves, and
reports nothing:

- `cost_check_interval.rs`: additions `[1, 2, 63, 64, 65, 127, 128, 129]`,
  commented "immediately before, at, and after interval boundaries", are
  `COST_CHECK_INTERVAL = 64` hard-coded. `dellingr::limits::COST_CHECK_INTERVAL`
  is public. (`mid_run_budget_reset_sees_a_flushed_meter` in `cost_golden.rs`
  does use the constant; the two files disagree in style.)
- `cost_golden.rs` `arith_error_after_batch_flush`: "65 loop adds ... spanning
  the 64-unit batch boundary". The source is a const string, so it cannot be
  derived, which is an argument for moving boundary cases out of the
  fingerprinted corpus and into a constant-derived test.
- `cost_determinism.rs` eviction test: `for i in 0..256` is
  `PATTERN_CACHE_ENTRIES` (public as `dellingr::limits::PATTERN_CACHE_ENTRIES`).
- `src/compiler/parser/tests.rs`: `too_many_upvalues_errors` (200 + 56, "limit
  255", should be `MAX_UPVALUES`, which is not even imported),
  `return_255_literals_uses_a_fixed_count` (255 rather than `MAX_EXPRESSIONS`),
  `syntax_depth_headroom` (see 1.10).
- `compiler_limits.rs`: 254/253/255 argument counts, 255/256 parameters,
  `TooManyAssignmentTargets { count: 256 }`, 300-entry constructors meant to
  cross `MAX_TABLE_BATCH_SIZE`, 256 sites meant to cross `MAX_CACHE_SLOTS`.
- `call_counts.rs`: `limit: 255` in `TooManyArguments`/`TooManyResults`
  matchers instead of `MAX_ARGUMENTS`/`MAX_RESULTS`.

The rendered error strings ("too many arguments (limit 255)") are fine as
message goldens; the boundary inputs are not.

Fix: derive every boundary input from the public constant. Enforcement: a
textlint rule in `brokkr.toml` over `tests/**` and `src/**` test code flagging
the literal values 63/64/65, 254/255/256 and 65534/65535 inside `.repeat(`,
`repeat_n(`, `0..N` and `1..=N` would catch most of these with few false
positives; it is a heuristic, not a proof.

### 2.5 The cost-golden module doc and CHANGELOG state stale facts

- `tests/cost_golden.rs`: "Fingerprint of `CORPUS` together with
  `COST_MODEL_VERSION`." The fingerprint also covers `ERROR_CORPUS`.
- `CHANGELOG.md` Unreleased says "`COST_MODEL_VERSION` is 7." in the
  register-VM entry and "`COST_MODEL_VERSION` is 8." in a later entry of the
  same section. The release will ship 8, so the first is false today. Three
  bumps (6, 7, 8) landed inside one unreleased cycle; a host only ever sees 5
  then 8. Per the documentation rule, the entries should say "a
  `COST_MODEL_VERSION` bump" rather than hard-code the number, with the final
  number stated once.

Not mechanically enforceable beyond a test that greps `CHANGELOG.md`'s
Unreleased section for "`COST_MODEL_VERSION` is N" and requires N to equal the
constant, which would be cheap and in the style of
`documented_opcode_charges_match_static_units`.

### 2.6 "The same cost on every build" has no cross-build check

`docs/determinism.md` promises the same cost "on a given build";
`docs/budget.md` and the `COST_MODEL_VERSION` doc say the version identifies the
`cost_used` "a given source produces", which is build-independent by
implication, and hosts are told they may key stored fingerprints on it. Every
cost test runs only in the dev profile: the release `[[check]]` shapes run
`tests = ["native_stack"]` only. Release is what ships and what records
replays. Overflow behaviour differs between the profiles (debug panics, release
wraps), and `analyze_cost` uses plain `+=` on `u64`. `save_golden.rs` already
documents one profile-dependent output (identical-code folding changes saved
function ids in release); nothing would notice a profile-dependent cost.

Fix: a corpus-wide runtime cost golden. `tests/run_examples.rs` already runs
every example through the CLI with `--quiet`; dropping `--quiet` and reading
`Cost used: N` (or a library-level runner) gives a per-example `cost_used`
table, stored in the cost fixture of 2.2 and fingerprinted with the version.
Run it in every `[[check]]` shape, including a release shape. That turns
"identical source, identical cost" from a claim into a check over the whole
corpus, in every feature and profile the repository builds.

### 2.7 The opcode-encoding fixture omits the replay-visible columns

`tests/fixtures/opcode_encoding.txt` pins opcode numbers, builtin slots, cache
operand encodings and jump encodings against the snapshot `format=`. It does
not pin each opcode's `Layout` (which fixes the reserved bits the verifier
rejects, so it is snapshot-visible) or its `OpcodeCost` class (which is
cost-model-visible). Reordering layouts or reclassifying costs does not touch
the fixture.

Fix: write `OP_ADD_RR=24 Abc Arithmetic` per line and add a
`cost_model=<N>` line; then a cost-class change fails this fixture and names the
version that has to move. The existing test holds it.

### 2.8 `reentrant_opcode_accounting.rs` and its opcode-coverage guard use different sources

`reentry_accounting_sources_cover_the_intended_dispatch_opcodes` in
`src/vm/frame.rs` exists to prove that the integration test's sources exercise
`CALL`, `TFOR_CALL`, `LEN`, `GET_FIELD`, `GET_TABLE`, `SET_FIELD`, `SET_TABLE`
and `SELF`. It compiles its own copies of the sources, which already differ
(`receiver:method()` in the guard, `indexed:method()` in the test; the guard
drops the `local prior = left + right` prefix). Edit the integration test's
source so it no longer emits `SELF` and the guard still passes. A guard keyed
on a copy fails open.

Fix: move the check into the integration test itself, using the public
`Program::disassemble()` text (opcode names are printed) to assert each case's
compiled source contains its opcode, and delete the unit-test copy. Held by the
test.

### 2.9 "Caches never change cost or behaviour" is checked on one source

`docs/determinism.md` "Inline caches": whether an instruction succeeds, what it
returns and costs, its stack need and metamethod depth "never depend on which
values the site saw before". The evidence is a handful of hand-written cases
(`bytecode_runtime.rs`, `global_write_cache.rs`, `plain_field_miss.rs`).

Fix: a test-infrastructure feature that disables every inline cache (always
take the miss path), and a differential test that runs the example corpus with
and without it and requires identical output, `cost_used` and error. That is a
mechanical proof of the doc claim over the corpus, and it would also catch
cache-key aliasing bugs.

### 2.10 Doc claims with numbers nothing derives

`docs/limits.md`: "`local x = ((...1...))` compiles up to 98 parentheses and
`return f(f(...f()...))` up to 49 calls", and the "Units per level" table. The
49 is asserted (computed from the constants) in
`deepest_nested_programs_body`; the 98 is asserted nowhere (the nearest test
uses 95 "below the limit"), and the per-construct unit table is asserted
nowhere. The machinery to check it exists: `deepest_accepted_program_until`
already finds the exact boundary per shape.

Fix: a unit test that measures the deepest accepted depth of each construct in
the table, derives units per level, and compares with the table parsed out of
`docs/limits.md`, in the style of `documented_opcode_charges_match_static_units`.
Held by the test.

### 2.11 `long_jumps_compile_and_run` keeps a rationale for a VM that no longer exists

`compiler_limits.rs`: "Far past the 16-bit jump range of the stack VM, inside
the 24-bit one." The stack VM is gone; 40,000-instruction jumps are nowhere
near any boundary of the 24-bit encoding, and
`jump_patching_rejects_offsets_past_the_encoding` in `codegen.rs` covers the
real boundary. The test now proves that a long-but-unremarkable chunk runs.
Either delete it or reword it as a smoke test; it costs three 40k-line compiles
per sweep.

### 2.12 Hard-coded instruction counts pin codegen, not cost

`static_analysis_counts_each_instruction_once` (`src/lib.rs`) asserts
`instructions == 10` for a loop. That is a codegen golden inside a cost test:
any codegen improvement breaks it while the cost property (each costed opcode
counted once) is unchanged. Move it to a codegen test, or assert only the cost
fields.

---

## 3. The test suite as a system

### 3.1 Fourteen integration binaries for about seventy tests

In scope alone: `cost_golden`, `cost_determinism`, `cost_check_interval`,
`stdlib_cost_golden`, `string_cost`, `call_counts`, `reentrant_opcode_accounting`,
`host_concat_count`, `lexer_diagnostics`, `compiler_limits`, `name_limits`,
`string_literal_errors`, `count_encoding`, `bytecode_runtime`. Each is its own
crate linking the whole library in debug, built and linked once per `[[check]]`
shape (default and snapshot, so 28 link steps for this scope), and each starts
as its own process on every `brokkr test <NAME>` run whether or not the filter
matches anything in it (the repository has about 78 top-level test files, so a
focused run starts about 80 binaries). Discovery is on (`autotests` is not
disabled), so every new file adds another.

The process-wide state these would share if merged is benign: `GMATCH_WRAPPER`
(a `OnceLock` compiled on first gmatch; its compile charges nothing) and
`NEXT_STATE_ID` in `vm/anchor.rs`. No test in scope writes environment
variables, installs a logger, or touches the filesystem.

Fix: two integration crates, `tests/cost/main.rs` and `tests/compiler/main.rs`,
with the current files as modules; set `autotests = false` and declare
`[[test]]` entries so a new top-level file is a deliberate act.
`native_stack.rs` and anything that can abort the process should stay separate.
Enforcement: `autotests = false` makes an undeclared file never compile, which
is its own hazard; pair it with a check in `tests/test_conventions.rs` that
every `tests/*.rs` and `tests/*/main.rs` is declared in `Cargo.toml`.

### 3.2 Misnamed and misplaced files make the scope hard to audit

- `call_counts.rs` tests argument/result count limits, not call counting or
  cost. `host_concat_count.rs` tests `State::concat`'s stack-index errors.
  Neither is cost accounting, though both read as such. `count_encoding.rs`
  holds the accept side of the 255 limits whose reject side is in
  `call_counts.rs`.
- `cost_determinism.rs` is documented as "Regression coverage for budget
  accounting and saved gmatch iterator costs".
- `string_cost.rs` carries `gsub_keeps_its_pattern_across_collections_in_its_replacement`,
  a GC-lifetime test with no cost assertion (and no proof a collection ran
  during the replacement).
- `lexer_diagnostics.rs` mostly tests `Error`'s `Display` prefix selection
  (`selected_caller_frame_does_not_borrow_the_innermost_column`).
- `src/compiler.rs`'s test module is `runtime_cache_tests` but holds the
  verifier corpus proof and the nesting-depth tests.
- `src/cost_meter.rs`'s tests include
  `gmatch_string_checks_preserve_relative_indexes_in_nested_frames`, an
  argument-index test for `State` methods that has nothing to do with the
  meter.
- `set_list_static_units_count_fixed_batches_only` (`instr.rs`) duplicates the
  two `set_list` assertions inside `documented_opcode_charges_match_static_units`.

Fix: rename and relocate during the merge in 3.1. Not mechanically enforceable.

### 3.3 Three tests prove the same chunk-nesting boundary

`chunk_nesting_limit_is_enforced_by_the_parser` (parser tests), the
`"function f() "` shape in `nested_statement_and_expression_shapes_body`
(`src/compiler.rs`) and `deepest_nested_functions_parse_on_the_parse_test_stack`
(`tests/native_stack.rs`). Likewise `fixed_call_argument_limit_preserves_dynamic_calls`
(`compiler_limits.rs`) overlaps `fixed_argument_limit_matches_its_error_message`
(parser tests), and `uncached_slots_templates_and_wide_set_field_at_execute`
overlaps `wide_literal_indices_and_uncached_slots_round_trip` in `save_state.rs`.
Each copy is cheap; together they are the drift risk of 2.8 three times over.

### 3.4 Feature sweeps run the compiler suite twice for nothing

Nothing in `src/compiler/` except `verify.rs` exports has a `snapshot` cfg, so
every parser, lexer, codegen and depth test runs identically in the default and
snapshot sweeps. The duplicate run of the cost goldens is useful (it shows cost
does not depend on the `snapshot` feature, which changes stdlib registration),
but only by accident, and only for the dev profile. Making that cross-shape
equality explicit (2.6) would justify the second run; the compiler tests could
be skipped in the snapshot shape with a `tests = [...]`-style exclusion if
brokkr grows one. Cost per run: a few seconds of debug test time per sweep; the
linking in 3.1 dominates.

### 3.5 The corpus is compiled three times per sweep

`compiler_examples_pass_bytecode_verification` (lib unit test),
`every_example_disassembles_into_well_formed_rows` (`tests/disassemble_corpus.rs`)
and `tests/bench_scripts.rs` each read and compile the whole examples corpus,
in addition to `run_examples` and the diff gate executing it. The corpus is
small (about 80 files, the largest the generated `parse/large_source.lua`), so
the cost is seconds, not minutes; the point is that one "compile every example"
pass could feed the verifier proof, the disassembly shape check, the
open-shape census and the cost golden of 2.6 together.

---

## 4. What the tests reach that they should have been handed

### 4.1 `COST_CHECK_INTERVAL` has no injection point, so the batching test needs a hand model

`cost_check_interval.rs` cannot run the VM at interval 1 and interval 64 and
compare, because the interval is a crate-private constant; it compares against
`immediate_flush_reference`, a hand-written model of a unit-at-a-time meter.
The model is independent and correct, but it only covers a straight line of
global-add statements. The property the doc states ("the interval does not
change the charged prefix or `cost_used` at `BudgetExceeded`") is general.

Fix: a test-infrastructure feature that sets the interval to 1, and a
differential over the corpus and the error corpus at a sweep of budgets
(the `fault-injection` sweep machinery in `error_boundary.rs` already walks
budgets). The production constant stays fixed; only the test build gets the
knob. Held by the differential test.

### 4.2 Two copies of the parse-test stack size, keyed on different features

`DEEP_PARSE_TEST_STACK_BYTES` in `src/compiler/parser.rs` and
`PARSE_TEST_STACK_BYTES` in `tests/common/stack.rs` both encode "2 MiB, more
when instrumented". The test copy widens for `hotpath` or `hotpath-alloc`; the
parser copy widens for `hotpath` only. `Cargo.toml` defines
`hotpath-alloc = ["hotpath/hotpath", "hotpath/hotpath-alloc"]`, which enables
the dependency's features but not this crate's own `hotpath` feature, so under
`--features hotpath-alloc` the parser functions are instrumented while
`cfg(feature = "hotpath")` is false: the unit-test parse threads stay at 2 MiB
with timing guards on every parser frame. The same single-feature spelling is
used in `tests/diff_test.rs` (corpus partitions compiled out only under
`hotpath`) and `tests/common/mod.rs` (`allow(dead_code)` only under `hotpath`).

Fix: `hotpath-alloc = ["hotpath", "hotpath/hotpath-alloc"]`, so one cfg
spelling covers both instrumented shapes, then delete the `hotpath-alloc`
alternatives; and keep the stack constant in one place the unit tests and
integration tests both read (a `#[doc(hidden)] pub` constant in `limits`, or a
`tests/common/stack.rs` that `src/` includes the way it already includes
`tests/common/examples.rs`). Enforcement: the feature implication is
structural; a textlint rule banning `feature = "hotpath-alloc"` in `cfg`s
outside `src/main.rs` and `examples/hotpath.rs` keeps it so.

### 4.3 Test-only instrumentation lives inside production `State`

`pattern_compilations` is a `#[cfg(test)]` field on `State` and a
`#[cfg(test)]` increment inside `cached_pattern_at`. That makes the unit-test
`State` a different layout from the integration-test and production `State`,
and it is why the cost tests in 1.9 cannot observe the cache. It also means the
unit-test build is not the build `native_bytes_per_level` measures, for whatever
that is worth.

Fix: same as 1.9, one feature-gated instrumentation surface used by both test
kinds, compiled out of production.

### 4.4 Tests poke private state to build preconditions

`gmatch_string_checks_preserve_relative_indexes_in_nested_frames` sets
`state.stack_bottom = 2` directly; `literal_pool_limits_match_their_error_messages`
fills `literal_indices.strings` without the matching `chunk.string_literals`
(the number half fills both). Both construct states production cannot reach,
so the tests can pass against invariants the real code never relies on, or
fail when an internal representation changes without any behaviour changing.
Prefer reaching the precondition through the public path (a nested Rust call
for the stack bottom; real literals for the pool, or the 65,536-literal source
the parser would actually see). Not mechanically enforceable.

---

## 5. Where the tests assume they run, and what they touch

### 5.1 Cost and compiler behaviour is only tested in the dev profile

Covered in 2.6 and 3.4: nothing in this scope runs in release except through
`native_stack.rs`. The parse-depth promise in `docs/limits.md` ("parsing at the
syntax budget fits a 2 MiB thread in both settings") is checked in release for
exactly one shape (`deepest_nested_functions_parse_on_the_parse_test_stack`);
the call chain, constructor chain, unary, `^`, parenthesis and statement shapes
run only in the dev unit tests. Release frames are usually smaller, but
inlining can make a particular release frame larger, which is exactly the
failure mode `native_stack.rs` exists for at runtime.

Fix: move the shape list into `tests/native_stack.rs` (or a sibling integration
test listed in the release `[[check]]` shapes) so it runs in both profiles.

### 5.2 The library unit tests read the source tree

`compiler_examples_pass_bytecode_verification` `include!`s
`tests/common/examples.rs` and walks `CARGO_MANIFEST_DIR/examples` at run time;
`persisted_encodings_match_fixture` and `documented_opcode_charges_match_static_units`
`include_str!` files from `tests/fixtures/` and `docs/`. All are rooted at the
manifest, so they do not depend on the working directory; they do mean the lib
unit-test binary is not relocatable and depends on files outside `src/`, which
`Cargo.toml`'s `exclude` happens to keep in the package. Acceptable; noted
because `tests/test_conventions.rs` scans only `tests/` and has no view of
`src/` test code at all (see 6.2).

### 5.3 Absolute paths in chunk names

`every_example_disassembles_into_well_formed_rows` uses
`path.display()` (absolute, host layout) as the chunk name. No assertion reads
it, so nothing breaks today, but any future golden of that listing would embed
the developer's directory. Use the manifest-relative path.

No test in scope writes files, reads the environment, sleeps, or depends on
locale, timezone, core count or the clock.

---

## 6. Lateral findings outside the scope

### 6.1 `brokkr test` always includes ignored tests, and one ignored test rewrites a fixture

`AGENTS.md`: `brokkr test <NAME>` "Always passes `--include-ignored`" and
`<NAME>` is a substring filter. `regenerate_crmath_golden` in
`src/crmath/tests/golden.rs` is `#[ignore = "rewrites tests/fixtures/crmath_golden.txt"]`
and calls `std::fs::write` on the tracked fixture. `brokkr test golden` (a
natural way to run `cost_golden`, `save_golden` and `stdlib_cost_golden`
together) also matches it and rewrites the fixture, and the research/-dependent
ignored crmath tests run too. The ignore attribute is the guard, and the tool
removes it. Fix: make regeneration an example or a script (as
`regenerate_save_golden` already is), never a test.

### 6.2 `tests/test_conventions.rs` does not see `src/`

It enforces "test file mutations must use the scratch helper" by scanning
`tests/` only. Unit tests under `src/` can write anywhere (6.1 is one), and the
"no CLI binary path outside common" rule likewise cannot see `src/`. Extend the
scan to `#[cfg(test)]` code in `src/`, or move the rule into a `brokkr.toml`
textlint with `test_code = "only"` over `src/**` and `tests/**`.

### 6.3 `clippy.toml` bans that cannot fire

`rand::rng`/`rand::thread_rng` and the `foldhash` `RandomState` types are
listed with `allow-invalid = true`. With `rand` no longer a dependency the
`rand` entries can never match anything; they would only fire if `rand` came
back under the same paths. That is the intended tripwire, but it is fail-open
by construction (clippy reports nothing about a path that does not resolve).
Worth knowing when reading the lint table as coverage.

### 6.4 The gmatch wrapper is process-global and its first-compile peak is order-dependent

`GMATCH_WRAPPER` is compiled by whichever gmatch runs first in the process.
`docs/limits.md` lists "the first call in a process also compiles the fixed
wrapper chunk there" as a deepest-point item of the native-stack envelope. In a
test binary, whether a given scenario is the first gmatch depends on test order
and thread scheduling, so that peak is covered only by luck. Compiling the
wrapper eagerly (at `open_libs`, or from a `LazyLock` forced in `State::new`)
removes both the order dependence and the envelope item.

### 6.5 Save bytes depend on the build profile

`tests/save_golden.rs` documents that a release build can fold identical Rust
functions (`unpack` and `table.unpack`) and then saves different function ids,
so its bytes differ from a debug save. That is a snapshot-content difference
between builds of the same version, which `docs/save-state.md` should state if
it is accepted, and which a release-shape run of the save golden would
otherwise flag. Also, `save_traversal_order_matches_golden_fixture`'s failure
message offers only "format or traversal change" as reasons, while a
`COST_MODEL_VERSION` bump also changes bytes 6..8 and fails it.

### 6.6 Three hand-maintained per-opcode operand descriptions

`compiler/verify.rs` (which operand is a literal, a cache slot, a nested
index), `disassemble.rs`'s `write_annotation` (the same mapping, written again)
and the dispatch loop each know which field of which opcode means what.
`opcode_rows!` already carries layout, cost and arithmetic function; adding
operand kinds there and deriving the verifier's pool checks and the
disassembly annotations from it would remove two of the three copies.
