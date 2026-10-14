# Hygiene hunt: stdlib (src/lua_std.rs, src/lua_std/, src/patterns/)

Scope read in full: `lua_std.rs`, `basic.rs`, `math.rs`, `string.rs`,
`string_format.rs`, `table.rs`, `patterns/{mod,errors,luapat}.rs`. Followed
out into `vm.rs` (cost meter, gmatch cache, fallback cache, string cap),
`vm_aux.rs` (`check_type`), `error.rs` (`ArgError` Display), `instr.rs`
(`Builtin`), `cost_meter.rs`, `save_state.rs`, README.

Not ranked. Each item says what is duplicated or assumed, whether copies have
already diverged, and what could hold the fix mechanically.

---

## A. Argument checking and error text: five owners of "bad argument"

### A1. Every `check_type` failure names no function, and the format has no quotes

`State::check_type` (vm_aux.rs) builds `ArgError { func_name: None, .. }`,
and `ArgError`'s Display (error.rs) renders `None` as `<anonymous>` and a
`Some(name)` *without* quotes: `bad argument #1 to <anonymous> (number
expected, got string)`. Nearly every library type check goes through
`check_type`, so `math.sin("x")`, `string.sub("a", {})`, `table.insert(1, 2)`
all say `<anonymous>`. Reference says `bad argument #1 to 'sin' (...)`.

Meanwhile the hand-written messages in the same library do quote:
`lua_std.rs` `exact_integer_argument` (`to '{function_name}'`), table.rs
`position_out_of_bounds`, math.rs `random` (x2), basic.rs `error`,
string.rs `gsub` arg 3, string_format.rs `format_arg_error`. So the two
formats have **already diverged** in one library: the same function can emit
`to 'insert'` (position check) and `to <anonymous>` (type check) depending
on which argument was wrong. No test pins `<anonymous>` (grep finds it only
in error.rs), so nothing notices.

Owners of the "bad argument #N to F (detail)" spelling today:
1. `ArgError` Display (error.rs) - unquoted name.
2. `exact_integer_argument` (lua_std.rs) - quoted, format! string.
3. `position_out_of_bounds` (table.rs) - quoted, format! string.
4. `format_arg_error` (string_format.rs) - quoted, format! string, its own
   "X expected, got Y" re-spelling of what ArgError already renders.
5. Literal strings: math.rs (3), basic.rs `error`, string.rs `gsub`.

Also `ArgError` is constructed by hand with `func_name: None` in
`vm.rs` (`memoize_gmatch_pattern`, `gmatch_subject_matcher_and_cost_meter`),
`vm/stack.rs` (2 sites), and string.rs `string_or_number_bytes`.

Fix: the function name is known at registration (`add_fn!("sin", ..)`).
Record the currently executing Rust function's registered name in its
CallInfo (the snapshot build already carries it as `"math.sin"`), have
`check_type` and friends fill `func_name` from it, quote it in Display, and
give `ArgError` a `detail: ArgDetail` enum (TypeMismatch, NoInteger,
OutOfBounds, IntervalEmpty, Custom) so every message has one renderer.
Enforcement: once `ArgError` carries the detail, a clippy
`disallowed_macros`/text rule can forbid the literal `"bad argument` outside
error.rs (brokkr's gremlins-style text scan or a test that greps `src/`);
plus a table-driven test that calls every registered stdlib function with a
wrong-typed arg and asserts the message contains `'<name>'`.

### A2. Wrong-but-type-correct values reuse the type-mismatch shape, producing nonsense

basic.rs `tonumber` (base out of 2..36), `select` (index 0 or too negative)
construct `ArgError { expected: Number, received: Number }`, which renders
`number expected, got number`. Reference says `base out of range` /
`index out of range`. `rawlen` says `table expected` for what is
"table or string expected". These are what A1's `ArgDetail` enum would make
unrepresentable: an `ArgError` with a type mismatch where the types match
should not be constructible (constructor that takes `expected` and the
received type, returns the `Custom` variant otherwise).

### A3. Integer-argument policy: one helper, bypassed by half the library

`exact_integer_argument` (lua_std.rs) exists and refuses non-integral floats
("number has no integer representation"). Used by `unpack`, `table.insert`,
`table.remove`, `table.concat`, `table.move`, and re-implemented in
string_format.rs (`integer_argument`, calls `exact_i64` itself) and inline in
basic.rs `error` (`level.fract() != 0.0`). Everything else truncates
silently with `as`:

- string.rs: `sub` (i, j `as isize`), `find` init, `match` init, `gsub` n.
- math.rs: `random` (m, n `as i64`; NaN becomes 0 and reports "interval is
  empty", 2.5 silently becomes 2).
- basic.rs: `select` index (`as isize`), `tonumber` base
  (`as i64` plus an `EPSILON` comparison - a third spelling of "is integral").

So `string.sub("abc", 1.5)` succeeds while `table.insert(t, 1.5, v)` refuses:
the policy has **already diverged** across sibling functions. Reference 5.4
refuses all of these.

Fix: one `arg_integer(state, n) -> Result<i64>` (and `opt_integer`) that does
type check + exactness + named error, used everywhere. Enforcement: enable
`clippy::cast_possible_truncation` (and `cast_sign_loss`) at module level for
`src/lua_std/` - the `as isize` / `as i64` on an `f64` read from the stack
would then need an explicit allow, which is the review signal. Also
`#[deny(clippy::float_cmp)]` already exists project-wide; the `EPSILON`
comparison in `tonumber` is the workaround that lint provoked.

### A4. Arg-count policy invented per function

`table.insert` and `math.random` each spell their own
`"wrong number of arguments to '<name>'"`; `math.min`/`max` hand-build an
`ArgError` for zero args that `check_type(1, Number)` would already produce
identically (dead duplicate code, two copies). Fold into the A1 helper.

### A5. Unrepresentable-subject and gmatch internal errors report a stack slot, not an argument

`memoize_gmatch_pattern` and `gmatch_subject_matcher_and_cost_meter` (vm.rs)
fill `arg_number: idx as isize + 1` where `idx` is the *absolute* stack index,
so the message names an argument number that has nothing to do with the
script's call. They are only reachable through an internal ordering mistake or
a doctored save, but if reached the text is actively misleading. They should
be `InternalError` (as the two neighbouring checks already are) rather than
masquerading as a script argument error.

---

## B. Cost charging: three meters, four copies of the same helper

### B1. `charge_cost` is defined three times, and inlined five more times

Identical bodies (`if state.cost_meter().consume(cost) { Ok(()) } else
{ Err(state.budget_exceeded_error()) }`) in string.rs, table.rs and
string_format.rs; `table.move` inlines the same thing five times. Nothing
keeps them in step except that they are short. Fix: `State::charge(cost)`
in vm.rs, and make `CostMeter::consume` `pub(super)` to cost_meter/vm so the
library cannot reach it. Enforceable by visibility.

### B2. Two charging APIs with different semantics - already diverged

math.rs and `table.insert/remove/pack` call `State::consume_cost`; the
string, format and concat/move paths call `cost_meter().consume`. With no
budget configured these differ: `consume_cost` always does
`cost_remaining -= cost` (starting at `i64::MAX`), whereas
`CostMeter::count_only` only bumps `cost_used`. So on an unbudgeted State,
`cost_remaining()` moves for `math.sin` and does not move for `string.upper`.
The two are also different shapes of "is budget exhausted" (one checks
`cost_remaining <= 0` on a State whose `cost_budget_configured` is false).
Fix: one charging entry point whose behaviour is decided by
`cost_budget_configured`, with `consume_cost` (the public host API) delegating
to it. A unit test asserting `cost_remaining()` is unchanged / changed
identically for both paths would pin it.

### B3. Per-function cost rules have no single table, and the README claim is not true of all of them

README "Budget" says data-dependent native work in string/pattern/table
libraries is charged. The actual rules are scattered as literals per closure
and are inconsistent:

- `find`, `match`, `gsub` charge `(s + p).max(1)` **and then** `p.max(1)`
  again (pattern bytes charged twice); `gmatch` charges `(s + p)` once at
  construction and `p.max(1)` again at first iteration via
  `memoize_gmatch_pattern` (vm.rs) - a fourth rule, owned by the VM, not the
  library.
- `string.len` free; `sub` charges output bytes but not a number-to-string
  coercion; `upper`/`lower`/`reverse` charge input bytes.
- `table.insert`/`remove` charge a flat 1 regardless of the shift (unless
  `table_insert_at` charges internally - the rule is then split across
  library and VM); `table.pack` charges 1 for any argument count;
  `unpack` (both spellings) charges nothing for up to 255 `get_table` calls,
  each of which may invoke `__index`.
- `print` (basic.rs) builds up to a 16 MiB message and charges nothing;
  `tonumber` parses up to a 16 MiB string free; `tostring`, `select`,
  `next`, `ipairs` free. These contradict the README sentence for
  "bytes examined or emitted".
- math.rs comments say "costs 1" on every function: 20 copies of a rule
  that is one constant.

Nothing answers "what does each stdlib function cost". `analyze_cost` cannot
see any of it (it walks bytecode). Fix: a declarative registration table per
library (`&[LibFn { name, func, cost: CostRule }]`, `CostRule::Flat(1)`,
`PerByte`, `PerElement`) for the flat parts, and a doc-tested README/reference
table generated from it. Enforcement: a test that iterates the registration
table and asserts each entry has a cost rule; a cost-snapshot test per
function (fixed inputs -> exact `cost_used`) so a rule change is a visible
diff.

### B4. gmatch's charge depends on cache state, so cost depends on GC schedule and on save/load

`memoize_gmatch_pattern` charges `pattern_len.max(1)` only on a cache miss.
The cache (`gmatch_patterns`, vm.rs) is cleared on every `gc_collect`, evicts
FIFO at `GMATCH_PATTERN_CACHE_ENTRIES = 256`, and is not saved - `load_state`
builds a fresh State. Consequences, by reading (worth a test before acting):

- Two runs of the same script with different `gc_set_threshold` settings, or
  different allocation history before the loop, can charge different cost for
  the same gmatch iteration (one hits, one misses after a collection). README
  "Determinism" promises same script and inputs => same cost per build; the
  GC threshold is a host knob, not an input the README names.
- A game saved mid-way with a live gmatch iterator stored in a global charges
  the compile cost again on its first resumed iteration, so a resumed replay's
  cost diverges from an uninterrupted one.

A cache is a wall-time optimisation; the cost it saves should not leak into
`cost_used`. Fix: charge compile cost per gmatch *iterator* (store a
"compiled" flag in the iterator's state table, which is saved) regardless of
whether the VM cache hit. Test: same script with GC forced between iterations
vs not, assert equal `cost_used`.

### B5. Cost/limit constants spelled outside the library

`GMATCH_PATTERN_CACHE_ENTRIES` / `_BYTES` live in vm.rs although only
string.gmatch uses them; `MAX_STRING_BYTES` (vm.rs) is correctly single-owner
and reached through `checked_string_growth` - that one is fine and is the
model.

---

## C. Limits spelled at more than one site

### C1. The 255-result limit: three spellings, two texts

- `unpack_values` (lua_std.rs): `span >= 255`, text "too many results to
  unpack" (twice in the same function).
- `select` (basic.rs): `u8::try_from(count)`, text
  "too many results (limit 255)".
- `vm/eval.rs`: same "too many results (limit 255)" literal.

The real owner is the `u8` return of `RustFunc` plus `RetCount`'s 255
sentinel (instr.rs). Whether a Rust function returning exactly 255 values
collides with the `All` sentinel is worth checking - `unpack` deliberately
allows 255 (`span >= 255` rejects 256+). Fix: `pub(crate) const
MAX_RESULTS: usize` and a `results_count(n) -> Result<u8>` helper next to
`RetCount`; unit test at 255/256.

### C2. Hand-counted capacity hints and headroom

- `new_table_with_capacity(24)` for math (22 entries registered),
  `(10)` for string (10), `(8)` for table (7). Hand-counted, already off by
  some for two of three.
- `reserve_stdlib_capacity` (vm.rs): `globals.reserve(Builtin::COUNT + 4)`,
  `heap.reserve(8, 96)` - magic numbers owned by the VM describing the
  library's shape.
- `STDLIB_SETUP_HEADROOM = 8` (lua_std.rs) is justified in prose ("four
  slots"); nothing checks it, the comment admits the per-push checks are the
  real backstop.

Fix: per-library `const FUNCTIONS: &[(&str, RustFunc)]`, capacities derived
from `.len()`; a test asserting the max stack depth reached during
`open_libs` is <= the headroom (the VM can track a high-water mark in test
builds).

### C3. Table library function count hard-coded in the VM: `[Val; 7]`

`TableLibraryFallbackCache.names: [Val; 7]` (vm.rs) and
`capture_table_library_fallback` does `live_string_keys().try_into()`; on
any other count it silently sets the cache to `None`. Adding `table.foo`
(or a host adding one before `open_libs` returns - not possible - or after,
which is handled elsewhere) silently disables a fast path with no failure
and no log: silent degradation. The count's owner is `open_table`. Fix: derive
the array length from table.rs's function list constant (`[Val;
TABLE_FUNCTIONS.len()]`), which makes the coupling a compile error. Test:
assert the fallback is `Some` after `open_libs`.

### C4. Builtin names: two lists in instr.rs plus the registration strings

`Builtin::from_name` and `Builtin::name` (instr.rs) each list all 19 names,
and basic.rs / the `set_global("math")` calls spell them a third time. A
mismatch between registration string and `Builtin` silently falls back to the
slow hash path. Fix: a single `const ALL: [(Builtin, &str); COUNT]` from
which both directions are generated (or a macro), and a test that every
`Builtin` is populated after `open_libs` (non-nil `builtins[i]`).

### C5. Snapshot IDs for gmatch iterators spelled twice

`"string.gmatch.iter"` and `"string.gmatch.empty_iter"` appear at the
push sites (`push_gmatch_wrapper` callers) and again at the up-front
`register_rust_fn` block. A typo in one makes a saved gmatch closure
unloadable. Fix: `const GMATCH_ITER_ID: &str`. Same for `"_G.__index"` /
`"_G.__newindex"` (one site each today, fine).

### C6. Pattern special characters known in two places

`is_plain_lua_pattern` (string.rs) lists 12 magic bytes; the pattern
compiler (luapat.rs) owns the actual grammar. Reference uses the set
`^$*+?.([%-`; the dellingr list adds `]` and `)` (harmless: just more patterns
take the slow path). If luapat ever grows a construct, the fast path will
silently treat it as literal. Fix: `patterns::is_plain(bytes)` exported from
the pattern module, next to the grammar; a test that every byte the compiler
treats specially is in the set.

---

## D. Registration boilerplate: one policy, six copies

The `#[cfg(feature = "snapshot")]` named-vs-plain registration fork is
written out in: basic.rs `add` closure, basic.rs `_G` metatable (twice),
math.rs/string.rs/table.rs `add_fn!` macros (three identical macros differing
only in the prefix), and string.rs `push_named_or_plain_rust_fn`. Fix: one
`State::register_lib_fn(table_idx, lib_prefix, name, func)` that does the cfg
internally. Enforceable structurally: make the two underlying methods
`pub(super)` to vm so lua_std can only use the unified one.

### D1. `expect` vs `?` on registration

The three `add_fn!` macros `.expect("... registration cannot fail")`, while
basic.rs propagates `?` and string.rs maps the snapshot `register_rust_fn`
failure to `InternalError`. `open_libs` is public (`State::open_libs`); the
string.rs mapping shows the snapshot registry *can* fail. If
`set_table_str_key_named_rust_fn` can fail for the same reason, the `expect`
is a panic on a host-controlled call. Use `?` everywhere (the function already
returns `Result`).

---

## E. Swallowed errors

### E1. gmatch iterator state read with `unwrap_or(0.0)`

`gmatch_empty_iter` (twice) and `gmatch_iter` read `pos`/`len` with
`state.to_number(-1).unwrap_or(0.0)`, and `gmatch_empty_iter` does not
`check_type(1, Table)`. Script code cannot reach the state table, but
README "Save state" says save files are user-editable input: a doctored save
whose iterator state has a non-number `pos` silently restarts iteration at 0
instead of refusing. Either validate in `verify_payload` or make the iterator
error. Also `unwrap_or` on a `Result` is the same smell `Result::ok()` is
banned for; `clippy.toml` could add `Result::unwrap_or` /
`Result::unwrap_or_default` to `disallowed-methods` with an allow at genuine
sites.

### E2. `tonumber(s, base)` swallows nothing but parses without bound

`parse_integer_with_base` accumulates in f64 over an unbounded digit string
(up to 16 MiB) uncharged (see B3). Not an error-swallow, but the only
uncharged O(n) parse in the library besides `tonumber(s)` itself.

---

## F. Shared mutable state whose safety is ordering-based

### F1. gmatch cache keyed by raw `StringPtr`

`gmatch_patterns: IndexMap<StringPtr, _>` (vm.rs) is safe only because the
sole path that frees strings, `gc_collect`, clears the map first. A second
collection entry point that calls `heap.collect()` directly (object.rs tests
already do) would leave keys that alias a new string at a reused slot, and
the iterator would run a *different* pattern. The invariant has two writers
(GC and the cache) and lives in one line at the top of `gc_collect`. Fix:
make the cache entries hold a GC-visible reference (key by interned string
content hash + compare bytes on hit), or move the clear into the heap's
sweep. The ordering invariant comments in
`gmatch_subject_matcher_and_cost_meter` ("caller-ordering invariants") are
the same shape: memoize must precede match. A typestate (memoize returns a
token that match requires) would make it structural.

### F2. `table_library_fallback` capture order

`State::open_libs` calls `lua_std::open_libs` then
`capture_table_library_fallback`; a host that calls the crate-internal
`lua_std::open_libs` path differently (e.g. `with_callbacks`) must remember
both. Worth checking `with_callbacks` does the same pair; either way the pair
should be one function.

---

## G. Documentation stating facts the code contradicts

- math.rs module doc and README "Determinism" (two owners of the same
  paragraph, already near-identical copies) list `pow` - no `math.pow` is
  registered - and `sqrt`, which IEEE 754 requires to be correctly rounded and
  is therefore bit-stable everywhere. The list should name what is actually
  registered and ULP-variable (`sin cos tan asin acos atan atan2 exp log`),
  and live in one place (README), with math.rs pointing at it.
- README "Compatibility divergences": `math.log(x, 2)` "follows Lua 5.2's
  general-base calculation"; the code's comment says base 10 is special-cased
  "as in reference". Both are probably true, but the README is the only owner
  of which reference version each stdlib function tracks - string_format.rs
  says "5.4 contract", math.rs random says "matches 5.4", unpack says
  "matches Lua 5.2". There is no per-function statement of target version.
- table.rs `sort` comment cites "(L18)", math.rs "(L5)", save_state "(L10)":
  labels from an audit list that is not in the tree (notes/ or nowhere).
  Durable code citing transient labels; reword to carry the reason inline.

---

## H. Dead or redundant code

- `base_unpack_values` (basic.rs) and `table_unpack_values` (table.rs) are
  one-line wrappers around `super::unpack_values`; register `unpack_values`
  directly. The only thing the wrappers might buy is a distinct fn address
  for the snapshot registry; if that is the reason (two registrations of one
  fn pointer under two names), it should be said in a comment, and the
  error text should then distinguish `'unpack'` (global) from
  `'table.unpack'` - today both say `'unpack'`.
- `math.min`/`max` zero-arg block duplicates `check_type(1, ..)` (A4).
- `LuaPattern::matches_bytes` is `cfg(test)` only - fine, documented.
- `patterns/mod.rs` doc says "wrapper around the vendored Lua pattern
  matcher" - it is now an in-crate rewrite (AGENTS.md says so); stale wording.

---

## I. Output channel

`print` correctly goes through `host_print`. No direct stdout/stderr writes
anywhere in scope. Nothing to report beyond B3 (uncharged).

## J. Host assumptions

None found in scope: no env, clock, filesystem, threads. `GMATCH_WRAPPER`
is a process-global `OnceLock` holding compiled bytecode shared across
States - safe (immutable, `Arc`), but it is the one piece of
cross-State shared state in the library; its bytecode identity is shared
between States, which matters if any snapshot/identity logic keys on the
`Arc` pointer.
