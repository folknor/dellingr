# Hygiene hunt: `surface` scope

Scope: `src/lib.rs`, `src/host.rs`, `src/error.rs`, `src/cost_meter.rs`,
`src/vm/save_state.rs`, `src/vm/save_state/verify.rs`, `tests/save_golden.rs`
and `tests/fixtures/`. I followed copies into `src/vm.rs`, `src/vm/frame.rs`,
`src/compiler/parser*.rs`, `clippy.toml`, `Cargo.toml`, README and CHANGELOG.
The findings are grouped by the hunt's question numbers, not ranked.

---

## 1. One value, one owner

### 1.1 The opcode cost table exists in four places (two are code)
- Runtime charges: `src/vm/frame.rs` `eval` dispatch (`add_cost!(.., 1)` per arm, `count` for `OP_SET_LIST`).
- Static analysis: `ScopeCost::analyze_chunk` in `src/lib.rs` keeps its own match over the same opcode list (`OP_ADD|..|OP_MOD`, the four `OP_NEW_TABLE*`, the six table-write opcodes, `OP_SET_LIST`).
- Prose: the doc comment on `frame.rs` `eval` ("Free operations: ... string operations ... Costs 1: ..."), and README "Budget".
- Nothing forces the two code copies to agree. If someone adds a costed opcode (e.g. a new fused `SET_FIELD` variant), it charges at runtime and is silently free in `analyze_cost`. The four `OP_NEW_TABLE*` variants have already been kept in step by hand.
- **Drift that has already happened:** the `eval` doc comment still lists "string operations" as free. Under cost model 2 they are charged, through `CostMeter` in the stdlib.
- Fix: define one `const fn static_cost(opcode) -> Option<CostClass>` in `instr.rs` (or a table indexed by opcode). The dispatch charges `static_cost(op)`, the analyzer sums it, and the doc comment points at it. Enforceable by a test that iterates every opcode value, compiles a one-instruction chunk and checks that the analyzer and the runtime agree. Making `static_cost` an exhaustive `match` over an opcode enum (not `u8` constants) would turn a missing arm into a compile error.

### 1.2 The budget-gate rule is implemented twice, and the copies have diverged
- `State::consume_cost` (`src/vm.rs`) always decrements `cost_remaining`, whether or not a budget is configured.
- `CostMeter` (`src/cost_meter.rs`) branches on `cost_budget_configured`. Its `CountOnly` arm increments `used` and **does not touch `remaining`**.
- **Fact, not prediction:** with no budget set, bytecode cost lowers `cost_remaining()` from `i64::MAX` but native string, pattern and table cost does not. So the public `cost_remaining()` reports a value that matches neither `budget - used` nor "unbounded". That value is also persisted by `save_state` (`cost_remaining` in `SavePayload`).
- The `consume` doc comment says it behaves "like `State::consume_cost`". That is true of the gate but not of the counter effect.
- The `add_cost!` macro in `frame.rs` adds a third piece of this policy: it peeks `cost_remaining().saturating_sub_unsigned(next) <= 0` to decide when to flush.
- Fix: make `State::consume_cost` delegate to `self.cost_meter().consume(cost)` and map `false` to `BudgetExceeded`, so there is one gate. Better still, collapse the three fields (`cost_remaining`, `cost_budget`, `cost_budget_configured`) into one `Budget { Unbounded, Finite { budget, remaining } }` enum owned by `cost_meter.rs`, so an unconfigured budget has no `remaining` to drift. A type makes the bad state unrepresentable.

### 1.3 `COST_MODEL_VERSION` was not bumped when the cost model changed
- CHANGELOG [Unreleased] "Constant folding": folded literal arithmetic "no longer execute or charge cost, so `cost_used` decreases for identical source ... re-baseline stored cost fingerprints".
- `cost_meter.rs` still has `COST_MODEL_VERSION = 2`. Its doc says the version identifies "the runtime cost model used by persisted state", and CHANGELOG 0.4.0 says it is exported "so hosts can identify the accounting model independently of the crate version".
- Strictly, this is a compiler change rather than a dispatch change. But the observable contract (`cost_used` for a given source) changed, and the one constant whose job is to say so did not move. A save written before the change and loaded after it carries `cost_used` and `cost_remaining` from the old model, and the version gate accepts it.
- This needs a decision only the authors can make: either the version covers "cost of a given source" (then bump it), or it covers only per-opcode charges (then reword the doc and CHANGELOG so hosts don't rely on it as a fingerprint key).
- Nothing enforces a bump. A golden test that pins `cost_used` for a fixed corpus next to an assertion on `COST_MODEL_VERSION` would fail on any cost change until someone touches both. That is the same pattern `save_golden.rs` uses for the format.

### 1.4 The save-format version has no single owner beyond the constant, and the fixture names restate it
- `FORMAT_VERSION = 6` in `save_state.rs` is fine as a constant. But the fixture file is named `save_golden_legacy_v6.bin`, the test message says "legacy v6 fixture present", and the current fixture is byte-identical to it (I checked with `cmp`).
- So "legacy" is currently the current format, and the name will be wrong the moment the version moves to 7, unless someone renames by hand.
- The CHANGELOG fusion entry ("The save format version is unchanged: an older binary ... rejects it cleanly as `InvalidBytecode`") shows the policy for when to bump is decided case by case in prose.
- Fix: name the legacy fixture after the version it pins only when it actually diverges from current. Add a test asserting `FORMAT_VERSION` equals the version bytes inside the current fixture. That test fails if someone bumps the constant without regenerating.

### 1.5 `tests/fixtures/save_golden.bin` is an unreferenced third copy
- It is byte-identical to both other fixtures (`cmp`), and no test, script or doc mentions it (searched `src`, `tests`, README, CHANGELOG, brokkr.toml). This is dead; see section 10.

### 1.6 Resource limits restated in error text and docs as literals
- `SyntaxError::TooManyExpressions`: "(limit 255)". `TooManyArguments`: "(limit 254)", while the check in `parser.rs` `checked_fixed_arg_count` is a bare `254`. `TooManyNestedFunctions`: "(limit 255)", while the check in `parser/func.rs` is `u8::MAX as usize`, spelled twice (once each in `parse_fndef_named` and `parse_fndef_method`).
- None of these have a named constant. The Display text and the check are independent literals. Fix: named consts in the compiler, interpolated into Display. Enforceable by a test that compiles source at limit+1 and asserts the message contains the constant.
- README restates "200 levels" (`MAX_SYNTAX_DEPTH`), "1000 frames" (`MAX_CALL_DEPTH`), "1,000,000 values" (`MAX_STACK_SIZE`) and "16 MiB" twice (`MAX_STRING_BYTES`). AGENTS.md restates 1000 and 1_000_000. A doc that has to state a number could be checked by a doc test (`assert_eq!(MAX_STRING_BYTES, 16 << 20)` placed beside the README claim via `#[doc = include_str!("../README.md")]` with a hidden doctest), or the docs could link the constant instead of copying it.
- `MAX_SYNTAX_DEPTH` is reused by the snapshot verifier (`validate_nested_graph`) as the cap on nested-chunk depth. That is a good single owner. But the parser bounds function nesting through syntax levels, not by counting chunk depth, so the verifier is borrowing a constant whose meaning differs: "nested chunk depth exceeds parser limit" is only approximately the parser's rule. Better to name a dedicated `MAX_CHUNK_NESTING` that the parser enforces directly.

### 1.7 The `"main"` scope name and the "anonymous #N" naming are spelled per site
- `analyze_cost` and `Engine::analyze_cost` each pass `"main".to_string()`. They also duplicate the whole body: parse, then `analyze_chunk`, then wrap. The free function could be `Engine::new().analyze_cost(&Engine::new().compile(src)?)`, or both could call one private helper.

### 1.8 Same-key equality for table keys exists twice on the load path
- `verify.rs` `resolve_key` normalises `-0.0` to `0` and rejects NaN and nil, which is how it decides whether two saved keys are duplicates.
- `save_state.rs` `runtime_table_key` exists for the same job at materialisation time, and the VM's table code has the canonical rule.
- If the VM's key normalisation ever changes (for example integer-valued floats), the verifier keeps accepting saves the VM would see as colliding, or the other way round. The comment on `validate_environment_deltas`'s test ("use runtime equality") shows the intent is a single rule. Make `resolve_key` call the VM's key canonicaliser on the decoded number instead of re-deriving it.

### 1.9 The stdlib-or-not decision has two owners
- `Engine { install_stdlib }` decides it for `new_state`.
- `State::load_state` ignores any Engine and decides from the save's `has_standard_environment` flag.
- An embedder using `Engine::raw()` who loads a save written by a stdlib State gets a stdlib State back, with no refusal. If this is intended, `load_state` should live on `Engine` and either check or override the flag explicitly.

### 1.10 `clippy.toml` reason text is stale
- The `rand::rng` and `rand::thread_rng` reasons say "Use State.rng (seeded StdRng) instead". The RNG is the in-crate `VmRng` (SplitMix64), and `rand` is no longer a dependency, as AGENTS.md says.

### 1.11 AGENTS.md says the deny list lives in `src/lib.rs`; it lives in `Cargo.toml`
- `src/lib.rs` denies only `clippy::unwrap_used` and `clippy::float_cmp` (both `cfg(not(test))`) and warns on `missing_docs`. The "long list" (`dbg_macro`, `todo`, `await_holding_*` and the rest) is the `[lints.clippy]` table in `Cargo.toml`.
- Two owners of lint policy is a finding in itself. `unwrap_used` and `float_cmp` could move to `Cargo.toml` too, except for the test exemption. If you keep the split, the doc should name both places.

---

## 2. Values nobody can find, change, or trust

- **There is no list of tunables.** The limits an embedder cares about are scattered: `MAX_STRING_BYTES` (pub, `vm.rs`), `MAX_CALL_DEPTH` and `MAX_STACK_SIZE` (private, `vm.rs`), `MAX_METAMETHOD_DEPTH` (private, `vm/metamethod.rs`), `MAX_SYNTAX_DEPTH` (`pub(crate)`, parser), `GMATCH_PATTERN_CACHE_ENTRIES` and `_BYTES` (`vm.rs`), `COST_CHECK_INTERVAL` (`frame.rs`, an associated const), and the bare 254/255 parser literals.
  - Only one of these is exported. A host that wants to show "max call depth" or pre-check a string length has to copy them.
  - A `pub mod limits` re-exporting all of them (or a `Limits` struct returned by `State::limits()`) would answer "what is the set?". A test could assert that every `const MAX_*` in `src/` is re-exported there.
- **`COST_CHECK_INTERVAL = 64` affects observable behaviour and has no injection point.** Its doc says "Higher values ... may overshoot budget more". The `add_cost!` macro also flushes whenever `remaining - next <= 0`, so the overshoot is bounded either way. But nothing documents whether the interval can change the point at which `BudgetExceeded` fires (and therefore `cost_used` at the failure). If it can, it is part of the determinism contract and belongs with `COST_MODEL_VERSION`. If it provably cannot, a property test should hold that, like the `consume_units` test in `cost_meter.rs`.
- **The limits have no injection point.** No test can lower `MAX_STACK_SIZE` or `MAX_CALL_DEPTH` to exercise the boundary cheaply. That is why `call_depth_exceeded_error` has to recurse a real 1000 frames, and why several doc comments in `error.rs` (`PrefixLocation::TraceFrame`, `ScriptError`) shape public types around debug-build stack size at depth 1000. A per-State `Limits` would make those tests cheap and would let the game tune them.

---

## 3. One channel, one implementation

- **`HostCallbacks::on_print`'s default implementation calls `println!` from library code** (`src/host.rs`). Every library consumer that forgets to override it writes to the process's stdout. That is a reasonable CLI default, but it is the library deciding that, not the CLI.
  - Fix: make `on_print` a required method, or make the default a no-op and have `main.rs` supply a `StdoutCallbacks`.
  - Enforceable with `clippy::print_stdout` / `print_stderr` denied crate-wide and `#[allow]`ed in `main.rs` only. The `debug_*` feature prints (`frame.rs` `debug_vm`, `object.rs` `debug_gc`, `parser.rs` `debug_parser`) would then need explicit allows too, which is the right signal.
- **`DefaultCallbacks`' doc says it is "suitable for CLI tools", but it is what `Engine::new_state()` and `State::new()` hand every embedder.**
- **`on_error` is not fired for snapshot failures.** `SaveError` and `LoadError` are a separate error family with no callback, and there is no logging anywhere on save or load. That is fine for a library, but it means the "single source of telemetry" claim in `on_error`'s doc does not cover the persistence path.

---

## 4. Errors

### 4.1 `SaveError::EncodeError(String)` merges VM bugs with resource limits
- In `save_state.rs`, about 20 sites return `EncodeError` with a fixed string.
- Some are invariant violations that indicate a bug: "unfilled object slot", "missing root slot", "missing table entry slot", "missing environment baseline", "environment object is not a table".
- Others are real resource refusals a host could act on: "too many strings", "too many objects", "too many upvalues", "too many chunks", "vector too long".
- A host cannot tell "your world is too big to save" from "report this as a bug" without matching on English text. Split them into `SaveError::Internal(&'static str)` and `SaveError::TooLarge { what, count }`. The strings are all literals, so `&'static str` also removes the allocation.

### 4.2 `LoadError` variants shed the context needed to act on them
- `UnsupportedVersion` and `UnsupportedCostModelVersion` carry neither the found nor the expected value. An operator holding a rejected save cannot tell an old file from garbage or from a newer build. Add `{ found: u16, expected: u16 }`.
- `CorruptArena` is returned from about 20 distinct checks in `verify.rs` (out-of-range string or object id, duplicate env token, duplicate deleted or upserted key, nil upsert, order-key mismatch, bad metatable kind, duplicate pointer id, ...) and from `object_ref` / `upvalues_ref`. Every one renders as "corrupt save arena". `InvalidBytecode` already shows the better shape: `chunk`, `instruction`, `reason`. Give `CorruptArena` a `reason: &'static str` and an index.
- `DecodeError(String)` messages ("truncated payload", "invalid bool", "trailing bytes", "string is not UTF-8") carry no byte offset, although `Decoder.pos` is right there.
- `verify.rs` `invalid()` maps a chunk index that overflows `u32` to `u32::MAX` (`unwrap_or(u32::MAX)`), which reports a fabricated chunk id. That index is bounded by a `u32` length prefix, so it cannot actually overflow. The fallback is dead and should be an `expect` stating that bound.

### 4.3 `load_state`'s setup closure cannot fail
- The signature is `setup: impl FnOnce(&mut State)`. But the operations a host does in setup are fallible: `register_rust_fn` and `set_global_named_rust_fn` return `Result<_, SaveError>`, and `push_*`, `new_table`, `get_global` and `open_libs` return `Result`.
- So the host must either `expect` (abort the process during a load of user-controlled input) or swallow. A duplicate-registration bug then surfaces later as `LoadError::UnknownFunction` that names the wrong cause. Make it `FnOnce(&mut State) -> Result<(), E>` and add a `LoadError::Setup(E)` (or a boxed error) variant.

### 4.4 `DuplicateFunctionRegistration` is a `SaveError` raised at registration time
- It is raised by `State::register_rust_fn` (`vm.rs`), which is neither save nor load. That is why `set_global_stdlib_rust_fn` has to `.expect(...)` it. A dedicated `RegistrationError` (or folding it into `error::Error`) would put it in the family that matches when it happens.

### 4.5 `SyntaxError::BreakOutsideLoop` renders a hard-coded line number (bug)
- `error.rs` Display: `"<break> at line 1 not inside a loop"`. Every misplaced `break` reports line 1, whatever line it is on. The real line is on the enclosing `Error`. Either drop "at line 1" or carry the line in the variant. A test with `break` on line 3 would catch it.

### 4.6 `SyntaxError::TooManyNestedFunctions`' doc describes the wrong rule
- The doc says "Source nests function definitions deeper than the VM accepts". The check (`parser/func.rs`) is on the **count** of direct child functions in one chunk (`nested.len() >= u8::MAX`), not depth. The Display "(limit 255)" agrees with the count reading. Depth is governed by `TooManySyntaxLevels`.

### 4.7 Error source and location are partly ambient
- `State::load` sets `self.current_source = program.0.source.clone()` as a side effect, "to track the source for callback context". That is shared mutable state whose value depends on which program was last *loaded*, not which one is *running* (see 7.1).

---

## 5. Policy invented per call site

- **Untrusted-length handling in the decoder is per site.**
  - `read_lua_string_bytes` caps lengths at `MAX_STRING_BYTES`.
  - `read_bytes` (used for the crate-version string, chunk names, sources, env tokens and `Fn` ids) has no cap other than the remaining bytes.
  - `read_vec` (not read in full) is documented as capping reservations at the remaining bytes.
  - So "how large may a decoded X be" is answered three ways. The crate-version string, `name` and `source` are host-irrelevant metadata of unbounded size, and `source` is later copied into every `StackFrame`. One `read_bounded(kind, max)` with a per-field cap table would put the policy in one place.
- **Duplicate detection is open-coded with fresh `BTreeSet`s** in `validate_environment_deltas` (three sets) and `validate_pointer_ids` (two sets), and through `ResolvedKey` in both. The policy (duplicate means `CorruptArena`, silently identical) is repeated per collection. A helper `insert_unique(set, key, what)` would also give 4.2 its reason string for free.
- **Version gating has no policy beyond exact equality.** `read_magic_and_versions` rejects anything that is not exactly `FORMAT_VERSION` and `COST_MODEL_VERSION`. That is a valid policy, but README and CHANGELOG describe it in prose per release. It should be stated once, next to the constants: "no migration; exact match; bump on any change that an old reader would misread".

---

## 6. Ambient dependencies

- `save_state` embeds `env!("CARGO_PKG_VERSION")` into every save. Two otherwise identical worlds saved by builds 0.4.0 and 0.4.1 produce different bytes. The module doc promises "Two saves of the same logical state are byte-identical", which holds only within one crate version. A host that content-hashes saves for replay dedup sees spurious differences. The comment calls the string "diagnostic metadata" and cites "(L10)", an identifier that resolves nowhere in the durable docs (see 10). Either make it optional or document it in the byte-identical claim.
- `register_rust_fn` keys the reverse map on `func as usize` (function address). The comment correctly notes ICF folding under release LTO. A consequence it does not state: two distinct host functions folded into one address both save as the lexicographically smallest id. After load in a build where they are *not* folded (debug, a different LTO decision), a value that was `b` comes back as `a`. The behaviour is identical, since the code was identical, but Lua `==` between them changes across the save/load boundary. This is a host-build dependency of the snapshot format. Worth a doc line, or a hard refusal when two ids share an address.
- The golden test (`tests/save_golden.rs`) uses `env!("CARGO_MANIFEST_DIR")` for its fixture paths. That is fine, and it is the right anchor.

---

## 7. Shared state whose safety is not structural

### 7.1 `State.current_source` is set by `load`, not by the running frame
- `State::load` (in `lib.rs`) writes `current_source`, which callbacks receive as `source` in `on_print` and `on_error`. If a host loads program A, then loads program B, then calls A's closure (both are on the stack, or A is reached through a global), A's prints are attributed to B.
- The value's correctness depends on call order, not on the frame that is executing. The running chunk's `Bytecode.source` is available on the `CallInfo` and frame. Derive it from there and delete the field.

### 7.2 `load_state` clears `registry` then runs `setup`, then `materialize_payload`
- The comment explains that anchors from setup are retained. Safety rests on `materialize_payload`'s allocators "never running GC" (module doc). That invariant has no type or assertion here.
- A `debug_assert` that the heap's GC epoch or counter is unchanged across `materialize_payload`, or a `NoGc` guard token the allocators require, would make it structural.

### 7.3 `env_tokens` is captured once at construction and used as the save-time authority
- This is good design, and the module doc explains why. But `build_env_reverse` clones the whole map per save. Note that `env_tokens` and `builtins` are two writers of "which object is `math`", and only construction keeps them consistent. That is fine as long as nothing ever re-runs `open_libs` on a live State. Is `open_libs` public? AGENTS.md lists it as fallible and public. If a host calls it twice, `env_tokens` and the live globals diverge. Worth checking who refreshes `env_tokens` in that case.

---

## 8. Unbounded growth

- `rust_fns_by_id` and `rust_fn_ids_by_addr` (`vm.rs`) grow with every distinct id registered and never shrink. That is bounded by host behaviour, so it is low risk, but there is no unregister.
- The decoder's metadata strings (5, first bullet) are unbounded per field, up to the save size, and `source` is cloned per stack frame on every error.
- `SaveBuilder.breadcrumbs` holds one entry per visited edge so that `UnregisteredFunction { reachable_from }` can render a path. That is linear in graph size, which is fine. The rendered path itself is also unbounded in length for deep graphs (one segment per level, each `Global(String)` cloned).

---

## 9. Secrets or personal data in diagnostics

- `SaveError::UnregisteredFunction { reachable_from }` renders a path built from global names and table keys, which can include user string keys. Fine for a game, but it is user-controlled content flowing into host logs unescaped and unbounded in length (see 8).
- `LoadError::UnknownFunction(id)` and `UnknownEnvObject(token)` echo attacker-controlled strings from the save file verbatim into Display. The README says saves are "user-editable input". A forged save can put arbitrary bytes (terminal escapes, megabytes) into an operator's log line. Truncate and escape them in Display.

---

## 10. Code that is no longer load-bearing

- **`tests/fixtures/save_golden.bin`**: unreferenced (searched src, tests, README, CHANGELOG, brokkr.toml) and byte-identical to the two referenced fixtures. It is presumably the pre-rename name of the current fixture. Safe to delete.
- **`ErrorKind::InvalidJump`**: constructed only in `vm/frame.rs` `jump` (plus its test). Since the snapshot verifier checks jump targets on load and the compiler emits only valid jumps, this path is reachable only if verification has a bug. It is not dead, but it is an `InternalError` in disguise with its own public variant. Consider folding it into `InternalError`.
- **The commented-out line in `SyntaxError::is_recoverable`** (`// matches!(self, Self::UnclosedString | Self::UnexpectedEof)`) is a dead alternative left in place. The REPL-continuation question it records (should an unclosed string be recoverable?) either belongs in `notes/todo.md` or should be deleted.
- **The `(L10)` references** in `save_state.rs` (two comments in `save_state` / `load_state`) cite an identifier from some past audit list. It is not in README, CHANGELOG or anything durable, so the code comment points at a note, which the project's documentation rule forbids. The comment's text is self-sufficient. Drop the tag.
- **`DefaultCallbacks` versus the trait defaults**: `DefaultCallbacks` is an empty impl that exists only because the trait has default bodies. If 3's fix makes `on_print` required, `DefaultCallbacks` becomes the one place the stdout default lives, which is better.
- **`Box<dyn HostCallbacks + Send>`** in `Engine::new_state_with_callbacks` and `State::load_state`: `HostCallbacks: Send` already, so the `+ Send` is redundant spelling. It is harmless, but it is a second statement of a bound that has one owner.
- **The `host.rs` module doc example uses `FleetCallbacks` and `fleet_id`**, which is game-project residue (AGENTS.md acknowledges it). Its `State::with_callbacks(Box::new(callbacks))` is the pre-`Engine` entry point. Point it at `Engine::new_state_with_callbacks`.
- **`lib.rs` crate-level example and `Engine` example**: the `Engine` doc example is `ignore`d, so it is never compiled and can rot. It looks compilable today and could be a real doctest.

---

## 11. Host assumptions and writes outside owned locations

- **The `regenerate` test writes into `tests/fixtures/`** (a tracked dir) via `create_dir_all` and `fs::write`. That is the right owner, and it is opt-in (`#[ignore]`). One wrinkle: it always writes `save_golden_current.bin` and never the legacy fixture, so after a format bump the round-trip test keeps loading the old fixture under its old name. That is correct behaviour, and only the naming issue in 1.4 remains.
- **The regeneration instructions say `cargo test --features snapshot --test save_golden -- --ignored regenerate`** in two places in `tests/save_golden.rs` (module doc and `expect` message). The project rule is `brokkr test`, never raw cargo. The equivalent `brokkr test regenerate` would also run it, because brokkr passes `--include-ignored`. Two copies of the wrong command.
- **`State: Send` has a compile-time witness in `lib.rs`; `Engine` and `Program` do not.** The `Engine` doc claims "`Engine` is `Send + Sync`" and `Program` "safe to share across threads". Both hold today (a bool, and `Arc<Bytecode>`), but if `Bytecode` ever gains a `Cell`-based cache the claim breaks silently. Extend the `const _` witness block with `assert_send_sync::<Engine>()` and `::<Program>()`. That is cheap and mechanical.
- **Native-cost charging assumes `usize` widths in places**, e.g. `pattern_len.max(1) as u64` in `vm.rs`. That is lossless on 64-bit, and on 32-bit `usize` to `u64` widens, so it is fine. No other host assumptions were found in this scope. The snapshot codec is explicitly little-endian and stores f64 by bits, so it is portable.

---

## Lateral notes (outside the scope's question)

- `ScopeCost::fmt_indent` prints "cost 0 (free)" only when `own_cost == 0 && nested.is_empty()`. A scope with zero own cost and costed children prints "cost 0 (own) / N (total)", which is fine. But `CostAnalysis::totals` computes `total_cost` by summing `own_cost` across the tree, while `ScopeCost.total_cost` is computed separately in `analyze_chunk`. That is a third copy of the same sum. Use `self.root.total_cost`.
- In `CostAnalysis`' Display, "Function definitions" counts `nested.len()` summed over the tree, and "Function calls" is counted but excluded from cost. Both are fine. `ScopeTotals` mirrors `ScopeCost`'s fields one for one and exists only to sum them. `ScopeCost` could implement `AddAssign` and drop the struct.
- `validate_quiescent` lists ten fields by hand. A new piece of in-flight VM state (another `*_bases` vector) would not be added there automatically. Putting "is quiescent" on `State` (next to the fields, in `vm.rs`) and having `call` / `eval` `debug_assert!` it at entry and exit would keep it honest.
