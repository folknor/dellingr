# Hygiene hunt: host surface, cost accounting and CLI

Scope: `src/lib.rs`, `src/host.rs`, `src/error.rs`, `src/limits.rs`,
`src/cost_meter.rs`, `src/main.rs`, `Cargo.toml`, `clippy.toml`, plus the
places those reach into (`vm.rs` budget fields, `save_state.rs` cost/name
fields, `instr.rs` counts, `patterns/luapat.rs` limits, `tests/cost_golden.rs`,
`tests/cli_limits.rs`, `docs/cli.md`, `docs/budget.md`).

Existing enforcement read first: `[lints.rust]`/`[lints.clippy]` in Cargo.toml
(package-wide denies incl. `disallowed_methods`, `unreachable_pub`,
`let_underscore_must_use`), `clippy.toml` (env/args/time/RNG/HashMap/hasher
bans), `src/lib.rs` (`unwrap_used`, `float_cmp` outside tests,
`missing_docs`), `brokkr check` (gremlins, clippy, tests, generator check).
That shows the build can enforce: path bans via clippy.toml, structure via
visibility (`pub(crate)`), and anything a test can assert. It has no
custom-text lint beyond gremlins.

---

## 1. One value, one owner

### 1.1 The "no budget" state has two representations and three spellings
`State` carries `cost_budget: i64`, `cost_remaining: i64` and
`cost_budget_configured: bool`. "Unbounded" is spelled as the bool *and* as
the `i64::MAX` sentinel: `State::new` sets both i64s to `i64::MAX` with the
bool false; `cost_remaining()` re-spells `i64::MAX` independently when the bool
is false; `CHANGELOG.md` documents the `i64::MAX` return. Every consumer has to
remember to test the bool first (`cost_meter`, `cost_batch_needs_flush`,
`gmatch_subject_matcher_and_cost_meter`, `lua_std/table.rs` sort's
`state.cost_budget_configured || honour_meta`). The copies have already
diverged in one place: the snapshot verifier and save_state test fixtures
build `cost_budget: 0, cost_remaining: 0, cost_budget_configured: false`, a
combination `State::new` never produces. Restore (`save_state.rs`, the
`state.cost_remaining = payload.cost_remaining` block) copies all four fields
without checking they are mutually consistent (e.g. `remaining > budget`,
configured=false with a non-MAX remaining), so a hand-built or corrupted
snapshot can install a meter state no API call could reach.
Fix: one `budget: Option<Budget { limit: i64, remaining: i64 }>` field (or
make `CostMeter` the owned value instead of a borrow-of-fields), so the bad
combinations are unrepresentable; `cost_remaining()` becomes
`map_or(i64::MAX, ..)` at exactly one site. Enforceable by type. The snapshot
encoding should follow the type (tag byte + payload), which also removes the
need for restore validation.

Related: there is no way to return a `State` to count-only once
`set_cost_budget` was called - the bool only ever goes false->true. If that
is intended, say so on `set_cost_budget`; if not, `clear_cost_budget` falls
out of the `Option` naturally.

### 1.2 `--analyze` caveat text is restated four times
The caveat lives as a hard-coded `eprintln!` string in `src/main.rs`, as prose
in the `analyze_cost` doc comment in `src/lib.rs`, in `docs/cli.md`
("The `--analyze` caveat states that...") and in `docs/budget.md`
("Static analysis"). They already differ in wording (the CLI says "not a
runtime bound or native-cost estimate", lib.rs says "neither a runtime lower
nor upper bound, and it is not a native-cost estimate"). A host printing a
`CostAnalysis` gets no caveat at all, because it is not part of the type.
Fix: `pub const ANALYSIS_CAVEAT: &str` next to `CostAnalysis` (or a
`CostAnalysis::caveat()`), read by the CLI and cited by the docs. Enforceable
by a CLI test asserting stderr contains the const.

### 1.3 Cost-category counters spelled at four sites
`ScopeCost` (7 counters), `ScopeTotals` (same 7 + `function_count`),
`ScopeCost::accumulate`, `ScopeCost::fmt_indent` and `CostAnalysis::fmt` all
enumerate the categories field by field, plus the `OpcodeCost` match in
`analyze_chunk`. Only the match is compiler-checked (exhaustive); adding an
`OpcodeCost` variant compiles fine if you forget `accumulate` or either
Display. The display labels also re-encode opcode knowledge
("Arithmetic (+,-,*,/,%,^)") that `instr.rs` owns.
Fix: `counts: [u64; OpcodeCost::COUNT]` (or an `EnumMap`) indexed by
`OpcodeCost`, with the label as a method on `OpcodeCost`. Then accumulate
is one loop and Display iterates. Enforceable by type.

### 1.4 `COST_MODEL_VERSION` is not tied to the golden table it guards
`tests/cost_golden.rs` detects `cost_used` drift and tells you to bump the
version, but nothing binds the table to the version: editing the expected
numbers without bumping passes. The version history is also kept in two
places (the doc comment on the constant, and `docs/budget.md` which defers to
it - fine) plus CHANGELOG.
Fix: key the golden table by version (`const GOLDEN_VERSION: u16 = 5;
assert_eq!(COST_MODEL_VERSION, GOLDEN_VERSION)` in the same file, or store a
digest of the corpus+expected values next to the version and assert both).
Then the only way to change the table is to touch the version line in the
same diff. Enforceable by test.

### 1.5 Limits reachable under several public names
`MAX_STRING_BYTES` is `dellingr::limits::MAX_STRING_BYTES`,
`dellingr::MAX_STRING_BYTES` (via `pub use vm::MAX_STRING_BYTES`, itself a
re-export) and `crate::vm::MAX_STRING_BYTES` (used by `parser.rs`,
`save_state/verify.rs` as `super::super::MAX_STRING_BYTES`).
`MAX_RESULTS` is both `limits::MAX_RESULTS` and `error::MAX_RESULTS`. Same
value, so nothing has diverged, but it is three doors to one room and callers
pick arbitrarily. Fix: one public path (`limits::`), drop the re-exports;
internal code imports `crate::limits`. Enforceable with a clippy.toml
`disallowed-methods`-style ban is not possible for consts; an `unreachable_pub`
style rule does not catch it either - the enforcement is simply deleting the
re-exports.

### 1.6 `16 * 1024 * 1024` spelled twice
`MAX_SOURCE_BYTES` and `MAX_STRING_BYTES` are both `16 * 1024 * 1024`
literals. If they are meant to be the same policy, derive one from the other
(as `GMATCH_PATTERN_CACHE_BYTES = MAX_STRING_BYTES` already does); if
independent, a comment saying so. Today a reader cannot tell.

### 1.7 Source-name length has two policies
`Engine::compile_named` / `State::load_string_named` accept a source name of
any length (the CLI passes the whole filename). `save_state.rs` caps the same
field at `MAX_CHUNK_SOURCE_BYTES = 256 KiB` (note the misleading name: it is
the chunk's source *name*, `ByteField::ChunkSource`, not its source text). A
state whose chunk was named with a long string compiles and runs, then fails
only at save time. Fix: one `limits::MAX_SOURCE_NAME_BYTES` enforced at
compile/load (refusal at the API boundary), read by save/verify. Rename the
save constant whatever happens. Enforceable by a test that a too-long name is
rejected by `compile_named`.

### 1.8 Resource limits living outside `limits.rs`
`limits.rs` says it is "Fixed resource and encoding limits" and that the
snapshot format validates against them, but replay-visible limits live
elsewhere: `patterns/luapat.rs` `MAXCCALLS = 200` (raises
`PatternErrorDetail::MatchDepthExceeded`, script-visible) and
`LUA_MAXCAPTURES = 32` (`TooManyCaptures`); `string_format.rs`
`MAX_FORMAT_LEN = 22`; `table.rs` `MAX_EXACT_KEY`; all the `save_state.rs`
`MAX_*_BYTES` caps; GC thresholds in `vm/object.rs`. Three independent `200`s
(`MAX_SYNTAX_DEPTH`, `MAX_METAMETHOD_DEPTH`, `MAXCCALLS`) look related and are
not. Conversely `limits.rs` holds things that are not limits at all:
`COST_CHECK_INTERVAL` (a batching tuning knob that `set_cost_budget`'s doc
promises is unobservable) and `GMATCH_PATTERN_CACHE_*` (an implementation
cache) are `pub`, which advertises tuning internals as contract.
Fix: split into `limits` (public, script-observable caps: everything that
produces an `ErrorKind`) and a `pub(crate) tuning` module (batch interval,
cache sizes, GC thresholds, capacity hints like `STDLIB_HEAP_TABLES`,
`HOST_GLOBAL_CAPACITY_HEADROOM`, the `Vec::with_capacity(256/64)` literals in
`State::new`). Then "what are the tunables" has one answer. Partially
enforceable: a test that every `ErrorKind` limit field is filled from a
`limits::` const is awkward; the module split itself is the enforcement.

### 1.9 Count sentinel `255`
`ArgCount`/`RetCount` encode the dynamic sentinel as `u8::MAX` in `instr.rs`;
`limits.rs` defines `MAX_FIXED_ARGS = u8::MAX - 1` and
`MAX_ARGUMENTS = u8::MAX` separately; panic messages hard-code "below 255".
The reasoning is documented and the values agree. Low risk, but the panic
strings are a third copy; format them from the const.

### 1.10 Line-number width
`Error::line_num: usize`, `StackFrame::line: u32`, `on_print(line: u32)`,
`Error::column: usize`. `limits.rs` says source size is capped so line
numbers fit u32. Pick `u32` everywhere on the public surface. Enforceable by
type.

---

## 2. Values nobody can find, change or trust

- Limits have no injection point by deliberate design (`limits.rs` header):
  replay and snapshot contract. That is a sound answer, documented. The
  consequence it names (tests exercise the real 1000-deep / 1M-slot values)
  is real; no further finding.
- The set of tunables has no single answer today (see 1.8).
- `COST_CHECK_INTERVAL` is aliased to an associated const in `vm/frame.rs`
  (`const COST_CHECK_INTERVAL: u64 = crate::limits::COST_CHECK_INTERVAL;`) -
  a second name for one value with no purpose; read the module const directly.
- CLI: all configuration is validated before any work (`parse_args` returns
  before the file is read; `--limit` + `--analyze` refused up front). Good;
  no finding.

---

## 3. One channel, one implementation

### 3.1 The library writes to stdout by default, from a trait default method
`HostCallbacks::on_print`'s *default body* is `println!`. Any host that
implements only `on_error` inherits stdout writes it never asked for, and a
game host forgetting the override silently prints to the game's stdout. The
comment defends it as "making it required would break every existing
HostCallbacks impl" - that is exactly the pre-1.0 API preservation the stance
rejects. Fix: make `on_print` required on the trait; move the `println!` into
`impl HostCallbacks for DefaultCallbacks` (or into the CLI as its own
callbacks type, since the CLI is the only stdout consumer). Enforceable by
signature. After that the only library stdout writes are the `debug_*`
feature dumps, which Cargo.toml documents as deliberate.

### 3.2 `println!` panics on a closed pipe
`DefaultCallbacks::on_print`, `main`'s `print!`/`println!` and every
`eprintln!` panic with "failed printing to stdout" on EPIPE:
`dellingr script.lua | head -1` on a script that prints more than one line
aborts with a Rust panic message and exit 101, not 1. For an embedder using
`DefaultCallbacks` in a service whose stdout is closed this is a process
abort from inside the VM on host-controlled conditions. Fix: CLI callbacks use
`writeln!(io::stdout().lock(), ..)` and turn an error into a terminal
condition (exit quietly, or surface through `on_print` returning `Result` so
the VM can fail the callback). Testable with a CLI test piping into a closed
reader.

### 3.3 CLI operator text assembled at each site
`"Error: "` prefix is spelled in both `report_error` and
`report_usage_error`; `"Cost used: {}"` is spelled inline in `main` and
asserted as a literal by `tests/cli_limits.rs` and documented in
`docs/cli.md`; the analyze caveat is inline (1.2). Small, but all CLI output
formats should live in one place (a tiny `cli_output` set of functions/consts)
that the test imports - the test cannot import from a bin crate today, which
is itself an argument for putting the CLI's argument parsing and rendering in
a `cli` module the test can reach (or in the lib behind a feature).

### 3.4 Error rendering ends with a newline when a traceback exists
`impl Display for Error` uses `writeln!` for every traceback frame, so the
string ends in `\n`; the CLI's `eprintln!("Error: {error}")` then prints a
blank line after every runtime error, and any host that appends its own
newline gets the same. Display output conventionally has no trailing newline.
Fix: join frames with `\n` without a trailing one. Testable.

### 3.5 Error-path silence in the CLI
When `--limit` is given and execution succeeds, nothing reports the budget or
remaining; the `Cost used` line is the only signal. Not a defect, noting that
`cost_remaining` is never surfaced to the CLI user.

---

## 4. Errors

### 4.1 Two "VM bug" kinds
`ErrorKind::InvalidJump { ip, offset }` and `ErrorKind::InternalError(String)`
both mean "compiler bug or corrupt bytecode"; `InvalidJump`'s Display
hand-copies the `"internal error: "` prefix that `InternalError` owns. A host
matching on "is this a VM bug" must know both. Fix: fold `InvalidJump` into
`InternalError` with a structured detail enum (`InternalDetail::InvalidJump
{..}`), one prefix owner.

### 4.2 `on_error`'s `source` duplicates `Error::source_name()`
`on_error(source: Option<&str>, error: &Error)` passes the frame source at
first report; `Error` also carries `source_name()` and a stack trace whose
first frame has a source. Three places to look for "where", which can
disagree (the doc on `Display` spends a paragraph explaining how splicing two
of them produced wrong output once). Pick one: either drop the parameter or
document precisely why it differs from `error.source_name()`.

### 4.3 Public fields and accessors for the same data
`Error` has `pub line_num`, `pub column` and also `line_num()` / `column()`
methods; `kind` and `stack_trace` are pub while `context` is private. Two read
paths, and the pub fields let hosts mutate an error after the VM marked it
reported. Pick accessors only (fields private), or fields only.

### 4.4 Public construction surface that is really internal
`error::results_count` (an ABI validator) and `ErrorKind::argument_detail`
are `pub` in the error module; `Error::new`/`without_location`/
`with_stack_trace` are public builders that let a host fabricate a stack
trace. Some of this is needed for `RustFunc` authors, but `results_count`
clearly is not a host concern. Audit what a host callback actually needs and
make the rest `pub(crate)`; `unreachable_pub` only catches items in private
modules, `error` is public so it does not help here.

### 4.5 `is_recoverable` has no in-tree consumer
`Error::is_recoverable` / `ErrorKind::is_recoverable` /
`SyntaxError::is_recoverable` exist for a REPL (append-more-input). There is
no REPL; only tests call it. It is a plausible host API, so not dead in the
strict sense, but nothing exercises it end-to-end. Either keep it with a
doc saying it is for host REPLs, or drop it.

### 4.6 Aborts on host-controlled input
`ArgCount::to_u8`/`RetCount::to_u8` assert, but are `pub(crate)` and only
reached from codegen; hosts are refused with a clean error. No finding. The
only host-reachable aborts found are the stdout/stderr panics (3.2).

### 4.7 CLI requires UTF-8 source
`fs::read_to_string` refuses a non-UTF-8 `.lua` file ("stream did not contain
valid UTF-8"), although Lua strings are byte strings and the VM handles bytes.
This follows from `load_string(&str)` being the only load API. A refusal, with
the path named, so operator-actionable; but it is a host assumption (source is
UTF-8) baked into the public API, not just the CLI. Worth a `load_bytes`.

### 4.8 Exit codes
Single exit code 1 for everything is documented and justified (CLI is a dev
tool). Except: panics exit 101 (see 3.2), so "every failure exits 1" in
`docs/cli.md` is already false for EPIPE.

---

## 5. Policy invented per call site

- Budget-is-configured check: every borrower re-implements "if configured
  then finite else count-only" unless it goes through `CostMeter::runtime`;
  `cost_batch_needs_flush` and `table.rs` sort test the bool directly (1.1).
  The `Option` type removes the per-site rule.
- Cost charging semantics exist twice: `State::consume_cost` delegates to
  `CostMeter::consume` (good, single owner), and `consume_units` restates the
  unit-loop semantics with a test proving equivalence (good). No finding.
- Snapshot restore of cost fields does no validation while other fields are
  verified (1.1) - the "how a stored value is validated" rule is applied
  per-field rather than per-type.

---

## 6. Ambient dependencies

- Library: clean. clippy.toml bans env, args, time, unseeded RNG, random
  hashers; RNG is `state.rng` seeded explicitly (default 0). Verified no
  `std::env` or `Instant` in `src/` outside `main.rs`.
- `main.rs` carries `#[allow(clippy::disallowed_methods)]` on `main` only;
  AGENTS.md says the CLI carries both `disallowed_methods` and
  `disallowed_types` allows. The CLI does not need the types allow today, so
  the code is right and AGENTS.md overstates; reword AGENTS.md to "scoped
  allows as needed".
- CLI resolves the script path against the process cwd - correct for a CLI.

---

## 7. Shared state and locking

- `State` is `Send` and deliberately not `Sync`, witnessed at compile time in
  `lib.rs`. `Program` shares `Arc<Bytecode>`, immutable. No locks in scope.
- The `reported` flag on `Error` is the only cross-site invariant
  (`mark_reported` gates `on_error` firing once). Its safety relies on every
  re-raise path carrying the same `Error` value rather than a rebuilt one;
  pub fields on `Error` (4.3) let a host clone-free rebuild lose it. Making
  fields private makes it structural.

---

## 8. Unbounded growth

- `cost_used` saturates at `u64::MAX` (count-only path, tested). Fine.
- The gmatch cache is bounded by entries and bytes. Fine.
- `on_print` receives a message bounded by `MAX_STRING_BYTES`. Fine.
- Source-name length is unbounded at compile time (1.7) - each chunk and each
  `Error`'s `ErrorContext` holds an `Arc<str>` of it, and `StackFrame.source`
  is a fresh `String` per frame per error, so a long name times
  `MAX_CALL_DEPTH` frames is copied per error. Cap the name, and make
  `StackFrame.source` an `Arc<str>` (it is already `Arc` in the chunk).

---

## 9. Code that is no longer load-bearing

- `Engine::compile` and `Engine::compile_named` ignore `self` entirely; the
  only Engine state is `install_stdlib: bool`. `analyze_cost(source)` free
  function and `Engine::analyze_cost(&program)` are two entry points to the
  same thing. `State::load_string(_named)` is a third compile path. The
  Engine facade adds a second way to do everything with one bool of payload.
  If the big-rewrite option is on the table: make compilation a free function
  (`Program::compile`), make `State::new`/`State::raw` the two constructors,
  and drop `Engine`; or move real configuration (budget defaults, RNG seed,
  callbacks factory) into Engine so it earns its place. What tells you it is
  thin: `compile` has no `self.` reference.
- `COST_CHECK_INTERVAL` alias in `frame.rs` (2).
- `DefaultCallbacks` is an empty struct whose whole behaviour lives in trait
  defaults (3.1); after 3.1 it carries the behaviour.
- `ScopeTotals.total_cost` is copied from `root.total_cost` only to be
  printed; harmless.
- `ErrorKind::InvalidJump` (4.1) - reachable only from corrupt bytecode, a
  duplicate category rather than dead.
- `hotpath` is a non-optional dependency (documented trade-off). Not dead.
- Docs drift: `lib.rs` crate docs say callbacks "handle errors"; `on_error`
  is notification only.

---

## 10. Host assumptions and writes

- CLI: assumes UTF-8 source (4.7) - refusal. Non-UTF-8 paths become the
  chunk name via `to_string_lossy`, so error messages show U+FFFD in place of
  bytes - silent degradation of the identifier, not a wrong answer.
- `-` is accepted as a positional (excluded from the unknown-option check,
  the conventional stdin spelling) but is then read as a file literally named
  `-`. Either implement stdin or reject `-` with a message; `docs/cli.md`
  says nothing about it.
- `tests/cli_limits.rs` writes scratch scripts into `CARGO_TARGET_TMPDIR`
  (cargo-designated for integration-test scratch, cleaned via Drop). That is
  the owner-sanctioned location; no finding.
- Library touches no filesystem, env, clock or locale. `MAX_CALL_DEPTH`
  correctness depends on host native stack size (documented in
  `docs/limits.md`): on a small-stack embedder thread the result is a native
  stack overflow abort, not a refusal. Documented, but not detected - there
  is no check of available stack, and that cannot be enforced portably.
- Float rendering depends on host libm for `^` (handled by not folding it,
  per `COST_MODEL_VERSION` 4 note). Outside scope, flagged as the one known
  host-dependence in replay output.

---

## Lateral

- `set_cost_budget` accepts negative budgets (CLI `--limit -1` too, tested).
  `BudgetExceeded { budget: i64 }` with `used: u64` - signed/unsigned
  asymmetry in one event. Fine by design ("i64 lets the operation complete"),
  but `remaining` going negative and `budget` being negative are different
  facts sharing a type.
- `analyze_cost`'s per-scope breakdown prints `cost 0 (free)` only when
  `function_calls == 0`; a zero-cost scope with calls prints `cost 0` plus
  the breakdown. Intentional per the comment; mention only because the two
  zero-cost renderings differ.
