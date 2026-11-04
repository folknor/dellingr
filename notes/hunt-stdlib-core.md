# Hunt: stdlib-core

Scope: `src/lua_std.rs`, `src/lua_std/basic.rs`, `src/lua_std/math.rs`,
`src/lua_std/table.rs`, `src/vm/rng.rs`, `src/crmath/`, `src/fp_env.rs`, plus
the boundaries they cross (`vm/table_ops.rs` sort and sequence helpers,
`vm/eval_control.rs` generic-for fast paths, `vm_aux.rs` argument checks,
`error.rs` prefix rendering).

Overall: the table, base and math libraries hold up well under partial
failure. Every metamethod path validates positions, key ranges and shift
distances before the first handler runs. Sort works on a detached copy or a
rooted scratch table, so a failing comparator leaves the source untouched.
Charges land before the work they pay for, and RNG draws happen only after the
charge and argument checks pass. I found no wrong-result bug in the library
bodies. What I did find is mostly contract drift: docs that went stale after
recent commits, a check that is narrower than the docs say, and a few
divergences missing from `docs/compatibility.md` even though that page claims
to list every one ("A difference not listed here is a bug").

## Findings

### 1. `docs/determinism.md` "Error labels" and the `ArgError` doc still describe name discovery from globals (doc contradicts code)

`docs/determinism.md`, section "Error labels", says generic Rust callbacks
checked with `State::check_any` / `State::check_type` "get a label from the
current global bindings or, with the `snapshot` feature, from the registered
function id", and that "the label can therefore differ between feature sets or
builds, and when globals are rebound".

The doc comment on `ArgError` in `src/error.rs` says the same thing in more
detail: "the VM looks the callback up only then: first the first current global
bound to the same function pointer, then, with the `snapshot` feature, the
callback's registered stable id."

Neither is true any more. Commit 912c2d5 ("Name host functions at creation
instead of discovering names from globals") made
`State::active_rust_function_name` (`src/vm.rs`) return
`self.rust_fns.name(function)`, the name fixed at `add_rust_fn` /
`register_rust_fn` time. The README ("argument errors raised while it runs name
it (`game.spawn`), however the script reaches it") and
`reference/error-reporting.md` ("Nothing is discovered at error time. The name
does not depend on which globals...") describe the new behaviour. So two
binding documents now contradict each other.

- Contract broken: `docs/` is "durable and binding"; README and
  `reference/error-reporting.md` say the opposite.
- Fix: rewrite the determinism.md bullets and the `ArgError` doc comment to
  say that the label is the name given when the function was created, the
  same in every build and feature set, and independent of globals.

### 2. The `safepoint-loops` check does not cover `src/lua_std.rs`, though the docs say it covers "every loop in the standard library"

`scripts/safepoint_loops.py` scopes itself with
`SCOPE_DIRS = ("src/lua_std/", "src/patterns/")`. The library's root module
`src/lua_std.rs` does not start with `src/lua_std/`, so the check never scans
it.

That file has library loops with neither a poll nor a decision comment:

- the `for index in i..=j` element loop in `unpack_values`;
- the `for` in `install_library_functions`;
- the `while` loops in the const fns `segment_offset` and `position_in`.

They are all bounded today: unpack's span is capped below `MAX_RESULTS`, and
the rest run over fixed lists or at compile time. The gap is in the guarantee,
not in behaviour. A future unbounded loop added to `lua_std.rs` (where the
shared argument helpers live) would pass `brokkr check`. The unpack loop shows
this already: its comment explains the charge decision but carries no
`bounded:`, and the check accepts it.

- Contract broken: `src/vm/interrupt.rs` ("The `safepoint-loops` script check
  ... holds every loop in the standard library ... to that") and AGENTS.md
  ("makes every loop in native library code poll or carry a ... comment").
- Fix: add `src/lua_std.rs` to `SCOPE_FILES`, and add `// bounded:` comments to
  the loops listed above (unpack: "at most MAX_RESULTS elements, checked above").

### 3. `unpack` runs the key-range check before the result-count check, so an over-long range reports the wrong error

In `unpack_values` (`src/lua_std.rs`), `table::check_sequence_keys(i, j, ...)`
runs before the `span >= MAX_RESULTS` test. So `table.unpack({}, 1, 2^60)`
(or `unpack(t, 1, 2^54)`) raises
`IntegerKeyRangeExceeded` ("unpack range exceeds the exactly representable
integer keys") instead of `TooManyResults`.

The real problem with that call is that it asks for about 2^60 results. Lua 5.4
reports "too many results to unpack", and dellingr would report the same if the
range sat inside +/-2^53.

`docs/compatibility.md` (Table library) says of the key-range refusal that "the
functions' other argument checks still apply first". The result-count limit is
the check that applies first in the reference, and here it applies second.

- Contract broken: compatibility.md's "other argument checks still apply first".
  It also gives the script author a misleading message.
- Fix: test `span >= MAX_RESULTS` (with the `checked_sub`) before
  `check_sequence_keys`. A range of up to 255 keys that reaches past 2^53
  still gets the key-range error, which is the case the refusal exists for.

### 4. `error(msg, 1)` / `error(msg, nil)` and `error(msg)` can render differently

`error` in `src/lua_std/basic.rs` maps "no level argument" to
`PrefixLocation::Current`, but an explicit level 1 or an explicit `nil` maps to
`PrefixLocation::TraceFrame(0)`.

`impl Display for Error` (`src/error.rs`) treats the two the same when the
stack trace has a frame 0. When the trace is empty, they differ:

- `Current` falls back to `source_name()` / `line_num`;
- `TraceFrame(_)` with no frame suppresses the prefix entirely.

So a host that calls the `error` builtin directly (no Lua frame on the trace)
gets a position prefix for `error("x")` but not for `error("x", 1)` or
`error("x", nil)`. In both references, `error(v)` and `error(v, 1)` are the
same call (`luaL_optinteger(L, 2, 1)`), and dellingr's own code reads an
absent level as 1 a few lines later (`unwrap_or(1)`). This is an edge case,
but the function's own name and default say the two calls are one.

- Fix: map level 1 (and absent or nil) to one `PrefixLocation`. Either fold
  `TraceFrame(0)` into `Current`, or have the no-argument path produce
  `TraceFrame(0)` too.

### 5. Divergences missing from `docs/compatibility.md`

The page says "Every deliberate divergence ... A difference not listed here is
a bug." The following are in the code but not on the page. None gives a wrong
result; each raises an error or skips a hook where a reference behaves
differently. So the defect is the list being incomplete, not the behaviour.
Each one needs a line in the doc, or a code change if it is not deliberate.

- `select`: dellingr treats only the exact string `"#"` as the count form.
  Both lua5.2 and lua5.4 test only the first byte
  (`*lua_tostring(L, 1) == '#'`), so `select("#n", ...)` returns the count
  there and raises "number expected, got string" here.
- `ipairs` ignores `__ipairs`, which Lua 5.2 honours. The README's "ignored
  fields" list and compatibility.md name only `__pairs`.
- `ipairs(v)` requires a table (`check_type_named(1, Table, "ipairs")`). Lua 5.4
  takes any value (`luaL_checkany`) and indexes it through `__index`, so for
  example `ipairs("abc")` iterates zero times there. Lua 5.2 agrees with
  dellingr.
- `table.sort` never raises "invalid order function for sorting". The heap sort
  quietly accepts an inconsistent comparator. This is stated only in a code
  comment in `table_sort` (`vm/table_ops.rs`). compatibility.md covers tie
  order and handler call order, not this.
- `table.sort` type-checks a non-nil second argument even when the length is 0
  or 1. Lua 5.4 checks the comparator only when `n > 1`, so
  `table.sort({}, 42)` succeeds there and raises here. This is harmless, and
  mentioned only so the list is complete.

### 6. `open_libs` comments claim more atomicity than the code has (minor)

The `STDLIB_SETUP_HEADROOM` doc in `src/lua_std.rs` says the up-front stack
reservation "keeps the operation all-or-nothing". Under `snapshot`,
`rust_fns.bind_catalog()` runs before `open_base`. The base, math, string and
table installs can still fail after the preflight: table growth, global
insertion, and string interning all go through fallible paths. If one does,
the catalog is already bound and some libraries are already installed. In
practice only an allocation failure gets there, so this is a wording issue:
the guarantee covers the stack cap and registry conflicts, not every failure.
- Fix: say "all-or-nothing with respect to the stack cap and registry
  conflicts".

## Checked and found consistent (no finding)

- `math.random`: charge, then argument checks, then draw. Rejected calls never
  advance the stream. A singleton range returns without drawing, as
  `docs/determinism.md` states. `random_f64` is `(u >> 11) * 2^-53` exactly
  (`MAX_EXACT_INTEGER` is `1 << 53`), so the result is in [0, 1). The i64
  rejection sampling is unbiased, and casting to f64 cannot leave `[m, n]`. The
  rejection loop is unbounded only in probability, and SplitMix64's full
  period rules out an endless rejection run.
- The `math` charges match `docs/budget.md` (a flat 1 each, charged before
  argument checks). `rawset` charges 1. Every function that budget.md lists as
  free is free.
- `math.modf`, `fmod`, `min`/`max` (NaN and signed zero), `deg`/`rad`, and
  `log` (base 10 special case, general quotient) behave as their comments and
  `docs/compatibility.md` describe.
- Table library: the raw and metamethod position rules match the module doc in
  `lua_std.rs`, including the wrapping checks for negative `__len`. Key-range
  and shift-distance checks run before any handler. `table.remove` at
  positions 0 and `len + 1` returns `t[pos]` and clears it.
  `table.move` uses the reference overflow guards and copy direction. A raw
  move charges per unit and copies exactly the elements it paid for.
  `concat` charges what budget.md lists, and so does `sort` (up front, at least 1).
- The `ipairs`/`next` fast paths in `instr_tfor_call` fall back to the Rust
  function on a miss that has a metatable, and on an invalid key. They match
  `base_ipairs_iter` / `base_next`.
- `fp_env`: the MXCSR and FPCR masks and decoders match the architecture
  manuals. Library calls skip the register read only while a catalog function
  is active, as `docs/determinism.md` says.
- `crmath`: I spot-checked `soft_fma` (alignment with a sticky bit, rounding,
  overflow and subnormal encoding), `hf`, `ln`'s special-case dispatch
  (signed zero, negative subnormals, -inf), and `exp`'s overflow and underflow
  thresholds. All are correct. There is no libm path. `fmod` and `floor`
  lower to exact operations, which `libm_symbols` allows by design.

## Lateral notes (not defects)

- `sequence_len` (`table.rs`) computes the raw border before it checks for
  `__len`, and discards it when a handler exists. That is wasted work on every
  metamethod-path call. `i64::try_from(..).map_or(i64::MAX, i64::from)` there
  (and in `table_sort`) is just an identity map with extra steps.
- The module doc of `src/lua_std/basic.rs` reads "Lua's Standard Library"; it
  is the base library.
- SplitMix64 seeds that differ by a multiple of the gamma constant
  `0x9e3779b97f4a7c15` give the same stream shifted by whole draws (seed
  `s + gamma` is seed `s` one draw later). That is acceptable under "not
  cryptographic", but a host that derives per-entity seeds as `base + k * gamma`
  would get overlapping streams. A sentence in `set_rng_seed`'s doc would head
  this off.
