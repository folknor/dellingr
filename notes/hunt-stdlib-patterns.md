# Hygiene hunt: standard library and patterns

Scope: `src/lua_std.rs`, `src/lua_std/*`, `src/patterns/*`, `src/vm_aux.rs`, plus
the VM helpers they lean on (`vm.rs` gmatch cache, `vm/table_ops.rs` sort,
`vm/eval_control.rs` iterator fast paths, `limits.rs`, `error.rs`). Read-only
review at 6c795d1; nothing was built or run. Unordered.

Existing enforcement worth knowing about when reading the "enforceable?" notes:
the `[lints.clippy]` deny list in Cargo.toml, the `disallowed-methods` /
`disallowed-types` lists in clippy.toml (env, clock, RNG, hash maps,
`Result::ok`), the `cast_possible_truncation` / `cast_sign_loss` deny at the top
of `lua_std.rs`, `unwrap_used` / `float_cmp` in lib.rs, and the gremlins scan.
There's no text rule (no grep-style check in `brokkr check`) beyond gremlins,
so any "spell it once" rule below needs either a type or a new check.

---

## 1. One value, one owner

### 1.1 Library capacity counts are hand-maintained in three places, two different ways
- `lua_std.rs`: `MATH_LIBRARY_CAPACITY = 22` and `STRING_LIBRARY_CAPACITY = 10`
  are hand counts. `TABLE_LIBRARY_CAPACITY = table::TABLE_FUNCTIONS.len()` is
  derived. So one library is structural and two are counted by hand.
- `vm.rs`: `STDLIB_INTERNED_TABLE_KEYS = 41` repeats the same counts again
  (22 + 10 + 7 + `__index`/`__newindex`). `STDLIB_HEAP_TABLES = 6` and
  `STDLIB_NON_BUILTIN_GLOBALS = 1` describe the stdlib layout from inside the VM,
  and the comment says "keep them in step with the registrations in `lua_std`".
- They agree today (I counted 20 math functions + `pi` + `huge`, and 10 string
  functions), so this is a prediction. They only size allocations, so drift costs
  a realloc and nothing breaks. That's also why nothing would notice it.
- Fix: give math and string the same `&[(&str, RustFunc)]` table that `table.rs`
  has (`MATH_FUNCTIONS`, `STRING_FUNCTIONS`, with constants listed separately),
  and derive every count, including the vm.rs key total, from those slices. Then
  the VM constants can go, or turn into `const` sums over the lua_std slices.
- Enforceable: yes, structurally, because the counts become `.len()`. Until then,
  a unit test that opens the libs and compares each table's key count to the
  constant would catch drift.

### 1.2 The f64 exact-integer bound is spelled locally
- `table.rs` defines `MAX_EXACT_KEY = 1 << 53`. `vm/rng.rs` writes `1u64 << 53`
  for a related but different purpose (the mantissa scale). `numeral::exact_i64`
  is the conversion owner but doesn't export the bound.
- `table.move` checks inline with a closure over `MAX_EXACT_KEY` and its own
  error string ("move range exceeds the exactly representable integer keys").
  `check_sequence_keys` formats `"{function} range exceeds ..."`, and its
  singleton rule (`exact_i64(first as f64) == Some(first)`) differs from move's
  plain range test. The two rules for one question have already diverged: move
  rejects a singleton key above 2^53 that is still exact (for example 2^54), and
  insert/remove/unpack/concat accept it.
- Fix: one `numeral::MAX_EXACT_INTEGER` and one `check_integer_key_range(first,
  last)`. `table.move` calls it once per side.
- Enforceable: only by convention. A test that `table.move(t, 2^54, 2^54, 1)`
  matches `table.unpack(t, 2^54, 2^54)` would pin the agreement.

### 1.3 Pattern and format limits live outside `limits.rs`
- `limits.rs` says it owns "fixed resource and encoding limits" and explains why
  they are constants (they're part of the replay contract). The stdlib keeps its
  own beside the code:
  - `luapat.rs`: `MAXCCALLS = 200` (pattern recursion), `LUA_MAXCAPTURES = 32`.
  - `string_format.rs`: `MAX_FORMAT_LEN = 22`, with no comment saying where 22
    comes from (lstrlib's `MAX_FORMAT - 10`). The width and precision digit cap
    is a bare `> 2` in `validate_directive`.
  - `basic.rs`: the `tonumber` base range `2..=36` is inline.
  - `table.rs`: `sort_through_metamethods` rejects `len >= i32::MAX` with "array
    too big", spelled twice. The raw sort path (`State::table_sort`) has no such
    cap, so the two sort paths disagree on whether a huge sequence is too big.
    The raw path is bounded by memory anyway, so in practice this is a dead check
    on one side.
- `MAXCCALLS` happens to equal `MAX_SYNTAX_DEPTH` and `MAX_METAMETHOD_DEPTH`
  (200), but it's independent, and that's right because the reference pins it.
  These values feed error behaviour scripts can observe ("pattern too complex",
  "too many captures", "invalid format (too long)"), so they belong to the same
  contract `limits.rs` describes. Someone tuning or auditing limits would look
  there and miss them.
- Fix: move them into `limits.rs` under a "reference-pinned library limits"
  heading. Don't make them tunable, just give them one home. Delete the
  metamethod-sort `i32::MAX` check, or add it to the raw path, so both sort paths
  have one length rule.
- Enforceable: not by lint. A doc-comment convention in `limits.rs` plus review.

### 1.4 Stdlib knowledge restated inside the VM
- `vm/eval_control.rs` imports `base_next` and `base_ipairs_iter` and fast-paths
  `TFORCALL` by function address. `lua_std.rs` re-exports them for that purpose
  only. The comment on `base_unpack_values` and `table_unpack_values` ("keep a
  separate callback address") exists because identity is by address.
  `active_rust_function_name` and snapshot reverse lookup are also keyed by
  address. That makes "function identity" an implicit contract between three
  subsystems, maintained by comments.
- Fix: a `StdFn` enum, or a registry entry carrying an explicit tag, that the
  dispatch loop matches on. Address identity would then only be an
  implementation detail of the registry.
- Enforceable: yes, by type, once dispatch matches a tag and not a pointer.

### 1.5 The gmatch iterator's state table is a schema spelled as string literals
- The keys `"s"`, `"p"`, `"pos"`, `"len"`, `"lastmatch"`, `"compile_charged"` and
  the integer slot `0` are spelled at each read and write in `string.rs`
  (`compile_charged` alone appears twice as a literal). The table goes into
  snapshots, so these names are persisted format. Renaming one silently breaks
  loading of in-flight iterators.
- The empty-pattern iterator uses a different shape (`pos`, `len`, no `s`/`p`,
  no `lastmatch`). Two schemas share the key `pos`.
- Fix: named `const` keys in one place, or better, a single typed
  `GmatchState { s, p, pos, lastmatch, compile_charged }` with `read` / `write`
  helpers. The empty iterator would then be a variant of it, not a second schema.
- Enforceable: by structure (one read helper, one write helper). The snapshot
  verifier could also validate the shape (see 5.2).

### 1.6 The replacement-template grammar is implemented twice
- `append_string_replacement` (first match, parsed per byte) and
  `compile_replacement_template` + `append_templated_replacement` (later matches)
  each implement `%%`, `%0`..`%9`, the "single capture means `%1` is `%0`" rule,
  and the invalid-index error. The "invalid use of '%' in replacement string"
  text is spelled twice inside the first one.
- The comment justifies the split: keep the error point identical, and charge
  exactly the same. Nothing structural enforces that the two produce the same
  bytes and cost. Unlike `find_subslice`, which has a naive twin test, there is
  no equivalence test.
- Fix: compile the template once, up front, into a result that can hold an error
  position (`Result<ReplTemplate, (offset_of_error, kind)>`), and have the
  applier raise the error lazily when the first application reaches it. That's
  the same trick `luapat` uses with `Item::Fail`. One grammar, and the same
  error-point semantics.
- Enforceable: by structure. Failing that, a twin test like
  `find_subslice_matches_the_naive_scan_at_every_budget`.

### 1.7 The two heap sorts must order equal inputs identically, and nothing checks it
- `vm/table_ops.rs::table_sort` (raw) and `table.rs::sort_through_metamethods`
  each contain the heap sort: sift-down with "smallest", a build loop, an extract
  loop, then a final reverse. The comment on the metamethod path says it uses
  "the same heap sort as the raw path, so equal inputs order identically". That
  is replay-visible (the order of equal keys, and the sequence of comparator
  calls), and the copies are kept in step by hand. The default comparators are
  also duplicated (`table_sort_less` vs `sort_less`, each with its own
  `TypeError::Comparison` construction).
- Fix: one generic heap sort over an accessor trait (`len`, `get(i)`,
  `swap(i, j)`, `less(i, j)`), with a `Vec<Val>` backend and a scratch-table
  backend.
- Enforceable: by structure. Failing that, a differential test that sorts the
  same input with and without an identity `__index` metatable, records the
  comparator call sequence, and compares.

### 1.8 Argument-error construction: one owner exists, four sites bypass it
- `vm_aux.rs::check_type` builds `ArgError { TypeMismatch, expected, received }`.
  Bypasses:
  - `basic.rs::arg_type_error`, a hand-built copy used once, by `setmetatable`.
  - `math.rs` `min` / `max`: a hand-built `ArgError` for "no arguments" that is
    exactly what `check_type_named(state, 1, Number, ..)` already produces for a
    missing argument (expected `Number`, received `None`). It's redundant code,
    duplicated twice.
  - `string.rs::check_string_or_number`, a hand-built `ArgError` that claims
    `expected: String`.
  - `string_format.rs::format`, which builds "string expected, got X" / "no
    value" as `ArgDetail::Custom` free text, where every other function uses the
    typed `TypeMismatch`. Same class of failure, different structure: a host
    matching on `ArgDetail::TypeMismatch` misses `string.format`.
- Fix: a `check_type_in(state, n, &[LuaType], name)` (or an `ArgDetail` with a
  set of expected types) in `vm_aux.rs`, used everywhere, with the four bypasses
  deleted.
- Enforceable: partly. If `ArgError` construction were `pub(crate)` only through
  constructor functions (private fields or a builder), a struct-literal
  `ArgError { .. }` outside `error.rs` / `vm_aux.rs` wouldn't compile.

### 1.9 Error text: typed in some places, free strings in others
- The same class of event goes through different channels:
  - "wrong number of arguments to 'X'": `math.random` and `table.insert` (twice)
    use `RuntimeError(String)`. Reference reports it as a plain error, but here
    each site formats the sentence itself.
  - "position out of bounds", "base out of range", "index out of range": go
    through `ArgDetail::OutOfBounds(String)`, where the payload is free text.
  - "table or string expected", "string/function/table expected": go through
    `ArgDetail::Custom(String)`.
  - "invalid capture index" in a gsub replacement is a `RuntimeError`, while
    "invalid capture index %N" in a pattern is a typed `PatternErrorDetail`.
    These are two messages for nearly the same thing, and the comment admits the
    split.
  - "invalid replacement value (a X)", "invalid value (X) at index N in table for
    'concat'", "object length is not an integer", "too many elements to move",
    "destination wrap around", "cannot change a protected metatable", "invalid
    conversion '...' to 'format'" (spelled twice in `parse_directive`), "invalid
    format (too long)", "specifier '%q' cannot have modifiers": all ad hoc
    `RuntimeError(String)`.
- Result: a host can't match on these without parsing text, and the wording that
  the diff tests pin against reference Lua is scattered over about 20 literals.
- Fix: a `LibError` enum in `error.rs` (or `ArgDetail` variants without `String`
  payloads) whose `Display` owns all the reference wording, with sites passing
  only data (the type, the index, the function name).
- Enforceable: yes. Removing `ArgDetail::Custom` / `OutOfBounds(String)` and
  making `ErrorKind::RuntimeError` non-constructible outside `error.rs` would
  make free text unrepresentable in library code.

### 1.10 Several anchor and plain-pattern predicates for one grammar
- `luapat::Compiler::compile` decides `anchored` from the first byte. Then:
  - `gsub` decides `anchored` again from `pattern.first() == Some(&b'^')`.
  - `gmatch` rewrites a leading `^` to `%^` by byte surgery, so it can reuse a
    compiler that treats `^` as an anchor.
  - `is_plain` and `is_plain_for_find` both derive from `PATTERN_MAGIC_BYTES`
    (good), but the "empty pattern" fast paths in find, match, gsub and gmatch
    are four separate special cases, each with its own return shape.
- The `gmatch` rewrite also charges one extra cost unit per iterator compared
  with the pattern as written (`charge_bytes` sees `len + 1`). That's a
  replay-visible quirk of an implementation detail.
- Fix: expose `CompiledPattern::is_anchored()` and have gsub read it. Give the
  compiler an `AnchorMode::{Honour, Literal}` flag for gmatch, which removes the
  rewrite and the extra byte. Decide in one place whether the empty-pattern fast
  paths are needed at all, since `str_match` already handles an empty item list.
- Enforceable: by structure.

---

## 2. Values nobody can find, change, or trust

### 2.1 Cost literals are spread across about 40 call sites, deliberately
- `string.rs` has a long comment saying the per-function literals are
  intentional and why a rule table wouldn't help: `analyze_cost` can't see
  native calls anyway. I accept the reasoning about where the literals live. The
  remaining problem is that there is no list: nobody can answer "what does
  `string.gsub` charge" without reading the function. The formula
  `(s.len() + pattern.len()).max(1)` plus `charge_bytes(pattern.len())` is
  repeated in find, match and gsub, while gmatch charges the second part lazily
  on first iteration, so the four pattern functions don't share one rule.
  `math.rs` justifies its 20 copies of `consume_cost(1)` the same way.
- `docs/budget.md` is where a designer would look. A charge table there (or a
  generated one) is the missing answer to "what is the set of tunables".
- Enforceable: a test that runs each library function on fixed inputs and
  snapshots `cost_used` into a golden table would make any change to a literal
  show up as a diff that needs a `COST_MODEL_VERSION` bump. That turns the
  comment's "needs a version bump" into a check.

### 2.2 `table.move` has two charging strategies keyed on `cost_budget_configured`
- With a budget, it charges 1 before each element. Without one, it charges the
  full count up front. If a raw `move_element` fails partway (stack cap, for
  example), `cost_used` differs between the two modes for the same script. A
  host flag that has nothing to do with the script changes the observable
  `cost_used` on the error path. That's minor, but it is policy chosen per call
  site.
- Same pattern as `find_subslice` / `append_bytes_units`, which use
  `consume_units` to batch with exact one-at-a-time semantics. `table.move`
  should use that too and drop the branch.
- Enforceable: a budget-sweep test like the `find_subslice` one.

---

## 3. One channel

- No direct stdout or stderr writes in scope. `print` goes through
  `State::host_print` to `HostCallbacks`. Good.
- The default `HostCallbacks::print` in `host.rs` does `println!`, outside scope.
  That's the right place for a default.
- `print` converts to `&str` lossily because the callback takes `&str`. Scripts
  can print arbitrary bytes and the host sees U+FFFD. That's a choice at the
  API boundary (`HostCallbacks::print(&[u8])` would be byte-exact), and it means
  the 3x expansion preflight in `print` and `error` exists only because of it.
  Worth reconsidering pre-1.0.

---

## 4. Errors

### 4.1 Swallowed: `to_number(-1).unwrap_or(0.0)` in the gmatch iterators
- `gmatch_empty_iter` (twice) and `gmatch_iter` (once) read `pos` / `len` with
  `unwrap_or(0.0)`. A missing or non-number field restarts the iteration at 0
  instead of raising. These tables round-trip through snapshots, so this is
  storage-sourced input with an unrecognised value silently defaulted.
- `gmatch_empty_iter` also doesn't type-check its argument 1 at all, unlike
  `gmatch_iter`.
- The existing `Result::ok` ban exists to stop exactly this, and `unwrap_or` on a
  `Result` gets around it.
- Fix: propagate (`?`) with an `InternalError` naming the field.
- Enforceable: clippy has no lint for `Result::unwrap_or`, but
  `disallowed-methods` can list `core::result::Result::unwrap_or` and
  `unwrap_or_default` the same way `ok` is listed, with scoped allows where a
  default is really meant.

### 4.2 Context shed
- `match_error` / `pattern_runtime_error` produce
  `ErrorKind::PatternError(detail)` with no function name. A "malformed pattern"
  from `string.gsub` and one from `string.find` look the same to a host. The
  location prefix comes from the Lua caller, so the line is there, but not which
  library call (or which argument) carried the pattern. Reference Lua shares
  this gap, but the typed error could carry `func_name` at no cost.
- `"invalid capture index"` (gsub replacement) doesn't say which index, though
  the code has `idx` in hand.
- `sort_less`'s comparison TypeError doesn't name `table.sort` as the caller.

### 4.3 Aborts the process on caller-controlled input
- `vm_aux.rs::is_none_or_nil` does `assert!(arg_number != 0)`. It's
  `pub(crate)`, the comment says crate callers only, and all current callers
  pass literals. That's acceptable, but `check_optional_type` is the path a
  future `pub` promotion would expose, and the public `check_any` / `check_type`
  already return `InvalidStackIndex` for 0. One function panics and its public
  siblings refuse. Make it return the same error.
- `basic.rs`: three copies of
  `isize::try_from(state.get_top()).expect("the value stack limit fits in isize")`
  in `tonumber` alone, plus more in `print`, `math.min` / `max` and
  `table.pack`. They can't fire, but it's the same invariant restated about 8
  times. A `State::top_isize()` accessor would own it.
- `string.find`: `u8::try_from(n - 1).expect(...)` relies on
  `LUA_MAXMATCHES <= 255`. It's sound, but that's an invariant of `luapat`
  asserted in `string.rs`. A `const _: () = assert!(LUA_MAXMATCHES < 256);`
  next to the constant would put it with its owner.
- `string_format.rs`: the `unreachable!` in `format_float` is reachable only if
  `format_argument`'s match changes. Splitting `Conversion` into
  `Conversion::Float(FloatKind)` would make it unrepresentable.

---

## 5. Policy invented per call site

### 5.1 Lua-index normalisation
- `string.rs` has `lua_start_index` / `lua_end_index`. `table.rs` has its own
  position rules (insert, remove, concat, unpack, move, each with its own bounds
  logic, intentionally matching 5.4's per-function quirks). `select` resolves
  negative indices itself. The string ones are local helpers that nobody else
  reuses. This is acceptable because the rules really do differ, but nothing
  documents the per-function rules in one place.

### 5.2 Snapshot-sourced library state: no validation owner
- The gmatch state table (1.5, 4.1) is library-private state that crosses the
  save boundary. Validation is lazy, at iterator call time, by ad hoc reads. A
  malformed table either defaults silently (4.1) or raises a type error naming
  `gmatch`. The snapshot verifier (`save_state/verify.rs`) doesn't know this
  schema. Same for the `_G` proxy metatable and the string metatable, which the
  library installs and the VM tracks as env tokens (`STRING_METATABLE_TOKEN` in
  vm.rs).
- Fix: library-owned state that crosses the save boundary should be a typed
  structure (a dedicated heap object, or validated on load), not a Lua table
  read with string keys.

### 5.3 "Keep original" truthiness in gsub
- The Table and Function arms of `append_gsub_replacement` each implement
  "nil or false keeps the original, string/number appends, otherwise error"
  verbatim, including the `invalid replacement value (a X)` message. Factor
  this into one helper that takes the value on top of the stack.

---

## 6. Ambient dependencies

### 6.1 libm transcendental functions: a host dependency that replays can see
- `math.sin/cos/tan/asin/acos/atan/atan2/exp/log` call `f64` methods, which
  lower to the platform libm. The module doc admits last-ULP differences across
  architectures and libm versions. `^` (VM, `powf`) has the same problem. This
  is the one place in scope where production logic reaches an ambient host
  facility whose output feeds replays. A replay recorded on glibc x86-64 and
  replayed on aarch64 or musl can diverge as soon as a script branches on a
  trig result.
- The clippy ban list covers clock, env and RNG, but not this.
- Fix: a pure-Rust, correctly rounded or at least fixed-implementation math
  library (for example the `libm` crate's software implementations, or
  `core-math` ports) called explicitly, then a `disallowed-methods` entry for
  `f64::sin`, `f64::powf` and the rest. That makes it enforceable the same way
  the RNG ban is. If you decide against it, `docs/determinism.md` should say
  plainly that cross-platform replay isn't guaranteed for scripts using these.
- Enforceable: yes, with clippy `disallowed-methods`.

### 6.2 Process-global `OnceLock` for the gmatch wrapper
- `GMATCH_WRAPPER` is a process-wide static shared across every `State`. The
  comment explains it's immutable. It's still the one piece of global state in
  the library, and it compiles lazily on first `gmatch` in the process, so the
  first caller in any process pays a compile, whichever State it is. That has no
  effect on cost, so it doesn't affect determinism. Better: wrap per State at
  `open_string` time, or drop the wrapper. A Rust closure upvalue could replace
  the Lua-source shim if `RustFunc` could carry upvalues, which is the deeper
  reason the shim exists.

### 6.3 `State::empty()` in `check_registry_compatible`
- The comment says constructing a scratch State "advances the anchor state-id
  counter". That's a global counter bumped as a side effect of a validation dry
  run. It's harmless for determinism by the comment's account, but a dry run
  shouldn't mutate global state. Listing the stdlib's (id, fn) pairs as data,
  which 1.1's function tables would provide, removes the need to build a scratch
  State at all.

---

## 7. Shared state

- `gmatch_subject_matcher_and_cost_meter` depends on `memoize_gmatch_pattern`
  being called immediately before, with no allocation in between: an ordering
  invariant between two calls, guarded by an `InternalError`. Merging them into
  one `with_gmatch_matcher(subject_idx, pattern_idx, |subject, matcher, meter|
  ...)` would make the ordering structural.
- The `_G` proxy relies on `__metatable = false` protection plus `getmetatable`
  and `setmetatable` honouring it (`raw_metafield`). That's two writers (basic.rs
  installs, basic.rs enforces) in one file. Fine.

---

## 8. Unbounded growth

- The gmatch pattern cache is bounded (`GMATCH_PATTERN_CACHE_ENTRIES`,
  `GMATCH_PATTERN_CACHE_BYTES`). Eviction is `shift_remove_index(0)` on an
  `IndexMap`, which is O(n) per eviction: 256 entries, so negligible.
- `find`, `match` and `gsub` copy subject and pattern into `Vec`s per call (the
  documented copy). They're bounded by `MAX_STRING_BYTES` and charged.
- `LuaPattern::new` is uncached for find, match and gsub. A script calling
  `s:find(p)` in a loop recompiles every time: up to 256 iterations of
  `match_class` per escape, uncharged ("structural"). The work is bounded per
  call, and gmatch already has a cache. Sharing that cache with
  find/match/gsub would be a real win, and the cost model is unaffected because
  the charge is the pattern length either way.
- No other growth in scope.

---

## 9. Code that is no longer load-bearing

### 9.1 Dead compatibility path: "Older snapshots have no cached key at integer slot 0"
- In `gmatch_iter`, the block that seeds slot 0 with `"compile_charged"` is for
  snapshots saved before slot 0 existed. That came in with fcc1f9e. Since then,
  `FORMAT_VERSION` has been bumped (f8a4704, now 7), and the loader requires
  `found == FORMAT_VERSION` exactly. A snapshot without slot 0 can't load, so
  the branch is unreachable from any loadable snapshot. A script can't reach the
  table either (it's an upvalue of the wrapper closure, and there's no `debug`
  library).
- Evidence: the git history of the comment and of `FORMAT_VERSION`, and the
  exact-match check in `save_state.rs`. Delete the branch and the comment.

### 9.2 Redundant hand-built errors in `math.min` / `math.max`
- See 1.8. The `n == 0` block duplicates what the next line produces. It's dead
  in effect: removing it produces the same error.

### 9.3 `check_any_named` on `_G.__index` / `__newindex`
- The proxy handlers check arguments 2 and 3 with the function names
  `"_G.__index"` and `"_G.__newindex"`. They only run when the VM dispatches a
  metamethod, which always passes 2 or 3 arguments. A script can only call them
  directly through `getmetatable(_G)`, which is protected and returns `false`.
  So these checks are unreachable from Lua, though a host could call them. Low
  value either way. Noting it so nobody cites them as precedent.

### 9.4 `#[cfg(test)] matches_bytes`
- A test-only wrapper, documented as such. It's live in tests, so this is fine.

### 9.5 Raw-path "array too big" asymmetry
- See 1.3. The `i32::MAX` check in the metamethod sort can only be reached
  through `__len` returning a huge number. That is reachable, so it's live, but
  the raw path has no equivalent. Keep one rule.

---

## 10. Where it assumes it runs

- No filesystem, environment, locale, clock or path use in scope. Character
  classes are fixed ASCII (`match_class`, `is_lua_whitespace`), not the C
  locale, so `%a` and similar are the same on every host. That's a deliberate
  and correct divergence from reference (which follows `setlocale`). It's worth
  listing in `docs/compatibility.md` if it isn't there already. The diff tests
  will only agree with reference under a C or UTF-8 locale where `isalpha` is
  ASCII-only, so a CI host with a Latin-1 locale could produce spurious diff
  failures on bytes 0x80 to 0xFF.
- libm (6.1) is the one real host assumption.
- `string.format("%p")` prints `format_pointer_id`, a State-assigned id, not an
  address. Good, deterministic.
- `format_float` relies on Rust's `{:e}` / `{:.N}` formatting, which is
  platform-independent. Good.

---

## Other things noticed

- **Bug (snapshot): the `ipairs` iterator has no registered id.** `base_ipairs`
  pushes `base_ipairs_iter` with `push_rust_fn`, not `push_lib_fn`, and nothing
  calls `register_lib_fn_id` for it. Compare `pairs`, which returns `base_next`,
  registered as `"next"`, and gmatch, which registers its iterators up front for
  exactly this reason. `docs/save-state.md` says an unregistered reachable
  `RustFunc` fails the save. So a script that keeps the iterator
  (`local it = ipairs(t)` captured as an upvalue or stored in a global, or a
  save taken while a generic `for` over `ipairs` is suspended in a host
  callback) can't be saved. I found this by reading the code and didn't run it.
  Fix: `register_lib_fn_id("ipairs.iter", base_ipairs_iter)` in `open_base`
  and `push_lib_fn` in `base_ipairs`. A test that saves a State holding an
  `ipairs` iterator would pin it.
- `base_ipairs_iter` reports errors as `'ipairs'`, while reference reports the
  iterator anonymously. That's harmless.
- `error(msg, level)` with a level above `u32::MAX` saturates through
  `unwrap_or(u32::MAX)`. That's fine, but it's another `unwrap_or` on a
  `Result`, the pattern 4.1 says to ban.
- `string.len` skips `check_string_or_number` on its fast path only when the
  value is a string, so it's consistent. Its error, though, says "string
  expected" for a table. Reference says the same, so this is fine.
- `tostring` and `string.format("%s")` both implement "a string bypasses
  `__tostring` unless the shared string metatable has one" separately
  (`string_has_tostring` check plus a `to_bytes` / `bytes_with_tostring_meta`
  split). `error` implements the opposite rule (always bypass). One
  `State::tostring_bytes(idx)` should own the first rule.
- `print` charges nothing, though it does work proportional to its output.
  That's consistent with the budget's "not a CPU meter" stance. Noting it only
  because it's absent from the list of deliberately free functions in the
  `string.rs` cost comment (`string.len`, `tostring`, `select`).
