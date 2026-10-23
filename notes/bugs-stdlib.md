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

# Standard library defects: base, table, math, string, patterns

Filed from the 2026-10-23 defect hunt at a22d9c0 (scopes `stdlib` and
`strings`, plus one lateral note from `crmath`). The hunters read the code
statically and built or ran nothing. The `stdlib` hunter quotes reference
behaviour from the 5.2.4 and 5.4 C sources (`lbaselib.c`, `lmathlib.c`,
`ltablib.c`), not from running the interpreters.

For most of the stdlib findings the `stdlib` hunter's yardstick is
docs/compatibility.md's opening claim: "Every deliberate divergence from Lua 5.2
and 5.4. A difference not listed here is a bug. Features that are absent
altogether are listed in the README under 'Won't implement'." Where a finding is
only a doc gap, the fix can be a code change or a new line in that file, but as
things stand the claim is false.

The `strings` hunter reports that the pattern engine, checked step by step
against 5.4 `lstrlib.c`, and `string.format`, checked against 5.4's per-conversion
flag and precision tables, gave no wrong-result defect.

## LIB-001 - The `_G` proxy's raw contents split from the real globals

From the `stdlib` hunter, rated medium.

**Mechanism.** `open_base` (basic.rs) makes `_G` an empty table whose protected
metatable has `__index` and `__newindex` handlers that forward to `GlobalEnv`.
Only the metamethod paths forward; the raw paths read and write the proxy table
itself:

- `pairs(_G)` and `next(_G)` iterate the proxy, which is empty, so they return no
  globals. Both reference versions enumerate every global.
- `rawget(_G, "print")` returns nil.
- `rawset(_G, "x", 1)` stores `x` in the proxy table, not in the global
  environment. After that, `x` (a bytecode global read) is nil; `_G.x` returns 1,
  because the raw hit means `__index` never fires; a later `x = 2` changes the
  global, but `_G.x` keeps returning 1; and `_G.x = 3` hits the raw key, so
  `__newindex` never fires, which updates the proxy and leaves the global alone.
  The script now has two different values both named `x`, and nothing reconciles
  them. Snapshots save the proxy table as an environment delta
  (docs/save-state.md), so the split survives a save and load.
- The proxy is the same table in every `with_restricted_env` environment. If a
  sandbox's whitelist includes `_G`, a raw key written there inside a restricted
  callback stays visible after the restriction ends. Any non-UTF-8 string key
  does the same, because `utf8_global_key` routes those into the proxy table.

**Claims broken.** compatibility.md lists only the `getmetatable(_G)` result and
the `setmetatable(_G, ...)` error as `_G` divergences. Empty `pairs(_G)`, nil
`rawget(_G, k)` and the `rawset` split are unlisted, so by that file's own claim
they are bugs. They also fail AGENTS.md's surprising-behaviour test: a script
that writes through `rawset(_G, ...)`, a common idiom for defining globals under
a strict-mode `__newindex`, silently defines nothing.

**Structural options**

- Make the global environment a real table, so that `_G` is that table and the
  `GlobalEnv` builtin mirror and versioning are layered over it.
- Or make the raw primitives proxy-aware: `rawget`, `rawset`, `next` and `pairs`
  special-case the proxy and route UTF-8 string keys to `GlobalEnv`.

The hunter calls the second a patch over a leaky abstraction; the first removes
the class of bug.

## LIB-002 - An explicit nil in an optional argument is rejected

From the `stdlib` hunter, rated low to medium. The `crmath` hunter reported the
`math.atan` and `math.log` cases independently as a lateral note.

**Claim broken.** The module doc of `src/lua_std.rs` states the library's
convention: "An optional argument that is absent or nil takes its default".
`opt_integer`, `check_optional_type_named` and `tonumber`'s `is_none_or_nil` all
follow it. None of the cases below is in compatibility.md.

**Mechanism.** Three functions test for an optional argument with `get_top()`
instead of `is_none_or_nil`:

- `error(msg, nil)` (basic.rs, the `error` closure): `get_top() >= 2`, so
  `arg_integer(2)` fails on the nil and raises `bad argument #2 to 'error' (number
  expected, got nil)`. Both references read the level with
  `luaL_optint` / `luaL_optinteger(L, 2, 1)`, so nil means level 1. The author's
  own error message is replaced by an argument error, which is the worst case:
  the message that explains the failure is lost.
- `math.log(x, nil)` (math.rs `log`): `get_top() >= 2` followed by
  `check_type_named(2, Number)` raises. Both references check
  `lua_isnoneornil(L, 2)` and return the natural log.
- `math.atan(y, nil)` (math.rs `atan`): the same pattern. 5.4 uses
  `luaL_optnumber(L, 2, 1)`, and 5.2 ignores the second argument.

The likely trigger is forwarding a possibly-nil value, as in `error(msg, lvl)` or
`math.log(x, base)`.

**Fix.** `error` should use `opt_integer(state, 2, "error")` with a default of 1,
and `log` and `atan` should use `check_optional_type_named`.

## LIB-003 - `table.concat` rejects a number separator

From the `stdlib` hunter. Low.

`concat` (table.rs) calls `check_optional_type_named(state, 2, LuaType::String,
"concat")`, which raises `string expected, got number` for `table.concat(t, 0)`.
Both references read the separator with `luaL_optlstring`, which converts a
number to its string, so they return `"102"` for `{1, 2}`. The elements in the
same function do accept numbers (`bytes_with_default_string_coercion`). The
"Coercion" section of compatibility.md covers only string-to-number coercion, so
number-to-string coercion of the separator is an unlisted divergence.

## LIB-004 - Other unlisted divergences: `__pairs`, `ipairs` through `__index`, `math.random(0)`

From the `stdlib` hunter. Low. None of these is in compatibility.md.

- `pairs` ignores `__pairs`: `base_pairs` pushes `next` without consulting the
  metatable. Both 5.2 and 5.4 call `__pairs` when it is present. The "Globals and
  metatables" section lists which metatable fields are implemented, which only
  implies that `__pairs` is not.
- `ipairs` reads through `__index` (`base_ipairs_iter` uses
  `get_table_unguarded`). That matches 5.4, but 5.2's `ipairsaux` uses
  `lua_rawgeti`. The table-library bullet that lists 5.4 metamethod semantics does
  not mention `ipairs`.
- `math.random(0)` raises `interval is empty`. In 5.4, `math.random(0)` returns a
  random integer covering all 64 bits. The module doc of `src/lua_std.rs` claims
  "`math.random` follows Lua 5.4's interval and error behavior", and this case
  breaks that claim. A full-width integer makes little sense with f64-only
  numbers, so the hunter expects the fix to be a doc line, but as written the
  claim is wrong.

## LIB-005 - Table-library charging is overstated in two docs, and the metamethod insert/remove loop has no cost bound

From the `stdlib` hunter, rated low to medium: a doc defect plus a lateral
hazard. LIB-006 and SRF-003 (notes/bugs-surface.md) cover nearby charging claims
with different mechanisms.

**Claims broken**

- README.md ("Budget"): "Data-dependent native work in the string, pattern, and
  table libraries is charged: bytes examined or emitted, table elements visited
  ...".
- docs/compatibility.md ("Cost-related differences"): "Native string, pattern and
  table work is charged per byte or element."

Both are false for `insert`, `remove` and `pack` (flat 1) and for `unpack`
(free). docs/budget.md documents all four as deliberate. The two general
statements need the exceptions spelled out.

**The hazard.** The gap matters most in the metamethod paths of `insert` and
`remove` (`insert_through_metamethods`, `remove_through_metamethods`). The shift
loop runs `first_empty - pos` or `size - pos` iterations, and that count comes
from a script-controlled `__len`, not from storage that exists:

```lua
local t = setmetatable({}, {__len = function() return 2^53 end})
table.insert(t, 1, 0)
```

This passes `check_sequence_keys` (the range ends at 2^53, inside the inclusive
bound), then runs about 9e15 iterations of raw get plus raw set of nil inside one
native call. The total charge is 1 plus the `__len` call's opcodes. A table with
only `__len` takes the metamethod path but never re-enters Lua in the loop, so no
opcode budget check ever runs.

The raw path cannot do this, because its length is bounded by real array
storage. The other walks are all bounded: `sort` charges `len` up front and caps
it at `i32::MAX`, `move` and `concat` charge per element, and `unpack` caps at
255.

Under budget.md this is a performance concern, not a cost-model defect, and
`while true do end` already shows the budget is not a termination guarantee.
Still, it is the one place where a single flat-charged library call does
script-chosen work with no bound. If the flat charge stays, the hunter suggests
capping the metamethod-path shift distance, for example at `TABLE_SORT_MAX_LEN`
as `sort` does, so that one call is bounded.

## LIB-006 - `rawset` writes a table for free while `t[k] = v` costs 1

From the `stdlib` hunter, rated low to medium (a cost-model gap).

`TableWrite` opcodes charge 1 (docs/budget.md, "Static opcode charges").
`rawset(t, k, v)` performs the same write and charges nothing, since `Call` is 0
and `rawset` never calls `consume_cost`. A script can fill tables without paying
the `TableWrite` cost by writing `rawset(t, i, v)` instead of `t[i] = v`.

budget.md has a "Free by design" list (`print`, `select`, `unpack`,
`string.len`, `tostring`) and a "Native charges" list. `rawset` is in neither,
and neither is the rest of the base library: `type`, `tonumber`, `rawget`,
`rawequal`, `rawlen`, `next`, `pairs`, `ipairs` and its iterator,
`getmetatable`, `setmetatable` and `error`. For all of these except `rawset`,
being free plausibly matches the design, because reads are uncharged opcodes
too. `rawset` is the exception: a free version of a charged operation, which
reads as an oversight unless documented as deliberate.

**Direction.** Either charge `rawset` 1 (a `COST_MODEL_VERSION` bump), or add it
and the rest of the base library to "Free by design".

## LIB-007 - Stale comment on global `unpack`

From the `stdlib` hunter. Trivial.

In basic.rs, the comment above `add!("unpack", base_unpack_values)` says
"Defaults to 1..#list, matching Lua 5.2's global unpack." The implementation
(`lua_std::unpack_values`) follows 5.4: the length comes from `__len`, and reads
go through `__index`, as the doc comment on `unpack_values` itself states. The
comment should describe the 5.4 semantics or simply point to `unpack_values`.

## LIB-008 - The README "Won't implement" list is incomplete

From the `stdlib` hunter. Low; a doc defect. CMP-004 (notes/bugs-compiler.md)
covers the same gap for `_ENV`, `\u{XXXX}` and local attributes.

compatibility.md delegates the list of absent features to the README, which
lists coroutines, io/os/debug, pcall/xpcall/assert, goto, `//`, bitwise
operators, `string.rep/byte/char`, arithmetic, comparison and concat
metamethods, implicit coercion, and long strings. These are absent from this
scope's libraries but not listed:

- `load`, `loadstring`, `dofile`, `require`, `collectgarbage`;
- `math.randomseed` (the host's `set_rng_seed` replaces it), `math.type`,
  `math.tointeger`, `math.ult`, `math.maxinteger`, `math.mininteger`, and 5.2's
  `math.pow`, `ldexp`, `frexp`, `cosh`, `sinh`, `tanh`, `log10`;
- `table.maxn`;
- the metatable fields `__pairs`, `__gc`, `__mode`, `__close` and `__name`.

`__mode` matters most: a script that builds a weak-keyed cache with
`setmetatable({}, {__mode = "k"})` gets no error, and a cache that never releases
its entries.

## LIB-009 - gmatch iterator state can tear, or skip a match, on a refused stack push

From the `strings` hunter, rated medium.

**Claims broken.** The 5.4 empty-match rule, which the lua_std.rs header and the
string.rs comments both promise. The project's own preflight-before-mutate
pattern: even `preflight_capture_bytes` is deliberately ordered before the
writes "so errors leave nothing half-done", but it preflights only cost, not
stack space.

**Mechanism.** `gmatch_iter` (string.rs) writes its persistent iterator state
before it has pushed what it returns, and nothing reserves the stack first:

```rust
state.push_string(gmatch_schema::POSITION_FIELD)?;
state.push_number(range.end as f64)?;
state.set_table_raw(1)?;                         // pos written
state.push_string(gmatch_schema::LAST_MATCH_FIELD)?;   // can be refused here
state.push_number(range.end as f64)?;
state.set_table_raw(1)?;                         // lastmatch written
... push_gmatch_capture(...)?                    // up to 32 pushes, each can be refused
```

Rust callbacks get no guaranteed headroom: `check_stack_slot` refuses at
`MAX_STACK_SIZE` globally. Two things can go wrong:

- **Torn state.** If the `LAST_MATCH_FIELD` push or its value push is refused,
  `pos` has advanced but `lastmatch` still holds its old value (or nil). On the
  next call, an empty match at the new `pos` is not rejected, because
  `lastmatch != pos`, so the iterator yields an extra `""` that the empty-match
  rule forbids. Example: `("abc"):gmatch("%a*")` yields `"abc"`, and then `""`
  instead of ending.
- **Skipped match.** If a capture push is refused after both fields are written,
  the match is consumed but never returned, and the next call resumes past it.

The State survives an error and can be reused. A gmatch closure stored in a
global is reachable from the next callback, and a snapshot taken afterwards saves
the bad state, which the verifier accepts. So this is a real continue-after-error
hazard. The `fault-injection` sweep checks surviving operands and State reuse,
but it does not re-drive the same iterator after an injected refusal, so this
escapes it.

**Fix.** `check_stack_space(2 + returned.len())` before the first
`set_table_raw`. Better, restructure:

- `lastmatch` is redundant: after any accepted match it always equals `pos`, and
  `pos` is never moved on a miss. Store one `pos` plus a "has matched" boolean (or
  encode "no match yet" as `lastmatch = nil` and drop the separate `pos` write),
  so there is a single table write and nothing can tear.
- Push the captures first and write the field last, so a refused push leaves the
  iterator untouched.

The snapshot schema (`gmatch_schema.rs`, `save_state/verify.rs`) would shrink
with it.

## LIB-010 - `gmatch_iter` per-call overhead

From the `strings` hunter, raised as a performance observation outside the
defect bar.

Each call re-interns four or five field-name strings through `push_string`, which
can run `maybe_collect` and so clear the pattern cache (see RT-017 in
notes/bugs-runtime.md), and does about six raw table reads and writes. Folding
away the redundant `lastmatch` field from LIB-009 also removes a write per
iteration.

## LIB-011 - gmatch errors are located in the synthetic wrapper chunk, not the script

From the `strings` hunter. Low.

**Claim broken.** reference/error-reporting.md opens with "Errors carry a
structured `ErrorKind`, source location...". gmatch is also inconsistent with
`find`, `match` and `gsub` raising the identical error.

**Mechanism.** Every other string function's runtime error (a malformed pattern,
budget exhaustion, an unfinished capture) is located at the calling script line
through `locate_in_frame` on the innermost Lua frame. gmatch's iterator is a
RustFunc called from the `GMATCH_WRAPPER` Lua closure (`return iter(state)`).
dellingr has no tail calls, so the wrapper frame is innermost when `gmatch_iter`
fails, and the error gets `string.gmatch` line 3 as its location. For
`for w in s:gmatch("(") do`, the headline location names a chunk the author never
wrote; their line appears only further down the traceback. The code comment says
the chunk name exists so errors "stay identifiable in traces", which accounts for
the traceback entry but not for taking over the primary location. No test pins
the `string.gmatch:` prefix, which suggests nobody decided this is intended.

**Fix.** Locate errors raised through the wrapper at the caller's frame, or
remove the wrapper. A RustFunc that can carry an upvalue or a bound argument
would remove the process-global chunk, the `call_unguarded` native level in
`push_gmatch_wrapper`, the snapshot bytecode for the wrapper, and this location
quirk at once. The hunter considers that structural improvement worth having on
its own merits.

## LIB-012 - `string.gsub` reports a missing argument 3 before checking arguments 1 and 2

From the `strings` hunter. Low.

**Claim broken.** The doc comment on `check_string_or_number` in string.rs: "an
argument must be validated before later arguments are read, so errors name the
arguments in the reference's order". `sub` follows the rule explicitly.

**Mechanism.** `gsub` calls `check_any_named(state, 3, "gsub")` before arguments 1
and 2 are type-checked:

- `string.gsub("x", {})` raises `bad argument #3 to 'gsub' (value expected)`,
  though the first problem is argument 2, a table where a pattern was expected.
  Reference 5.4 reports `#2 ... string expected, got table`.
- `string.gsub()` raises the `#3` error, where reference reports `#1`.

The message points the author at the wrong argument, so this is not merely a
wording difference. The `check_any_named(3)` call is also redundant: the
`repl_type` check further down already rejects an absent argument with
`StringFunctionOrTableExpected`, which is the reference's own message.

**Fix.** Drop the call.

## LIB-013 - The `pattern_matcher` doc comment sits on `gsub_empty_pattern`

From the `strings` hunter. Low; a doc defect.

In string.rs, the block that begins "The matcher for argument 2 of `find`,
`match` or `gsub`. A string pattern comes from the State's compiled-pattern
cache; a number coerced to `pattern` has no interned key and compiles fresh..."
sits directly above `#[inline(never)] fn gsub_empty_pattern`. Rustdoc and IDE
hovers therefore describe `gsub_empty_pattern` as the pattern-matcher helper,
while `fn pattern_matcher`, further down the file, has no doc comment.

**Fix.** Move the comment.

## LIB-014 - AGENTS.md names string.rs helpers that do not exist

From the `strings` hunter. Low; a doc defect.

AGENTS.md's "Patterns" paragraph says "`string.rs` has helpers
`is_plain_lua_pattern` (fast path) and `gsub_replacement`." Neither exists:

- The plain-pattern classifiers are `crate::patterns::is_plain` (gsub) and
  `is_plain_for_find` (find, which treats `)` as literal), in
  `src/patterns/mod.rs`.
- The replacement dispatcher is `append_gsub_replacement`, with
  `append_templated_replacement` / `append_function_replacement` and the
  `ReplTemplate` compiler.

notes/optimizations.md also still refers to `is_plain_lua_pattern`. Per the
documentation rule, reword AGENTS.md to describe the roles rather than re-pinning
names that drift.

## LIB-015 - Pattern compilation does about 256x its byte charge in host work

From the `strings` hunter, raised as a performance observation outside the
defect bar.

Uncharged pattern compilation is `O(pattern bytes x 256)`, because every `%x`
escape, inside or outside a bracket, loops over all 256 bytes. A pattern of up to
`MAX_STRING_BYTES` can therefore cost about 256 times its byte charge in host
work. It repeats on every call when the cache misses: every GC clears the whole
pattern cache (RT-017 in notes/bugs-runtime.md), and a number-typed pattern
always bypasses the cache. The `class_for_escape` comment documents this, and it
fits the "structural work is free" framing. `match_class` results per escape
letter could be a 22-entry static table of `ByteClass`es, which makes each escape
a copy.

## LIB-016 - Small string-library observations: dead check, double `%s` charge, capture-index wording

From the `strings` hunter, raised outside the defect bar.

- **Dead check in `bracket_class`** (luapat.rs). `if class_start == close` can
  never be true: the scan loop always advances `scan` past `class_start` before it
  can break with `close = scan`. The `[]` and `[^]` cases it seems to guard are
  already rejected by the loop running off the end. Harmless, but the
  comment-free check suggests a contract it does not enforce.
- **`string.format` `%s` charges twice.** `%s` charges its rendering in
  `format_argument` and charges the padded result again in `append_output`.
  budget.md describes both ("bytes appended to the output and bytes of string
  arguments consumed"), so this is consistent; `%s` simply costs about 2x per
  byte.
- **`InvalidGsubCaptureIndex` drops the index.** It displays as `invalid capture
  index` without the `%N` it carries in its typed field, while the pattern-side
  `InvalidCaptureIndex` renders `%N`. The hunter does not count it as a defect
  under the wording rule, but calls it an easy consistency win.

## LIB-017 - Global-key allocation and per-call `ipairs` registration

From the `stdlib` hunter, raised as performance observations outside the defect
bar.

- `utf8_global_key` allocates a `String` (`name.to_owned()`) on every `_G.k` read
  or write that reaches the proxy handlers. A borrow-then-lookup that does not
  allocate, or interned-string keyed global access, would remove it.
- Under `snapshot`, `base_ipairs` calls `push_lib_fn("ipairs.iter", ...)`, which
  runs `register_rust_fn` (a string hash lookup) on every `ipairs(...)` call,
  although `open_base` already registered the id. `base_pairs` uses a plain
  `push_rust_fn(base_next)` and relies on the reverse address lookup. Pushing the
  already-registered function directly in both would be cheaper and consistent.

## LIB-018 - Undocumented replay-relevant properties: libm `fmod`, and degenerate `math.random` ranges

From the `stdlib` hunter, raised outside the defect bar.

- `math.fmod`, and `numeral::lua_modulo` (the VM's `%`), use Rust's f64 `%`,
  which LLVM lowers to a libm `fmod` call on most targets. `fmod` is exact, so
  every conforming libm agrees and replays are safe. It is still a host-libm
  dependency that docs/determinism.md does not mention next to its "computed
  in-crate, never by the platform libm" wording.
- `VmRng::random_range_i64` returns early without drawing when `end <= start`,
  so `math.random(1)` and `math.random(m, m)` do not advance the stream, while
  5.4 always draws. This is deterministic and fine, but it is a replay-visible
  property of the RNG draw sequence that docs/determinism.md does not state. If
  it ever changed, every replay that hits a degenerate range would diverge.

## LIB-019 - `table.sort` raw path overwrites comparator writes and bypasses a mid-sort metatable

From the `stdlib` hunter, raised outside the defect bar.

On the raw path, a comparator's writes to `t[1..n]` are overwritten by the final
`set_array` writeback, and a metatable the comparator adds mid-sort is bypassed
by that raw writeback. Reference Lua's behaviour here is also unspecified, so the
hunter does not count it as a defect. compatibility.md's sort bullet documents
the call-order difference only for the metamethod path, not this one.
