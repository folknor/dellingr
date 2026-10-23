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
`strings`). The hunters read the code
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

**Deferred (user, 2026-10-23).** Not to be picked up by a resolution wave until
the user reopens it.

**Structural options**

- Make the global environment a real table, so that `_G` is that table and the
  `GlobalEnv` builtin mirror and versioning are layered over it.
- Or make the raw primitives proxy-aware: `rawget`, `rawset`, `next` and `pairs`
  special-case the proxy and route UTF-8 string keys to `GlobalEnv`.

The hunter calls the second a patch over a leaky abstraction; the first removes
the class of bug.

## LIB-005 - Table-library charging is overstated in two docs, and the metamethod insert/remove loop has no cost bound

From the `stdlib` hunter, rated low to medium: a doc defect plus a lateral
hazard. LIB-006 covers a nearby charging claim with a different mechanism.

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
**Decided (user, 2026-10-23):** keep the flat charge, cap the metamethod-path
shift distance (at `TABLE_SORT_MAX_LEN`, as `sort` does), and spell out the
flat and free exceptions in the README and compatibility.md charging
statements.

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

**Decided (user, 2026-10-23):** `rawset` charges 1, the same as a `TableWrite`
opcode. That is a `COST_MODEL_VERSION` bump, a refreshed cost golden
fingerprint and a CHANGELOG entry (script costs change). budget.md should then
list `rawset` among the native charges, and add the rest of the base library,
which only reads, to "Free by design".

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

## LIB-019 - `table.sort` raw path overwrites comparator writes and bypasses a mid-sort metatable

From the `stdlib` hunter, raised outside the defect bar.

On the raw path, a comparator's writes to `t[1..n]` are overwritten by the final
`set_array` writeback, and a metatable the comparator adds mid-sort is bypassed
by that raw writeback. Reference Lua's behaviour here is also unspecified, so the
hunter does not count it as a defect. compatibility.md's sort bullet documents
the call-order difference only for the metamethod path, not this one.

## LIB-020 - `string.gsub` charges before it rejects a bad argument 3

Lateral from the first resolution wave's review. Dropping the early argument-3
presence check (so errors name arguments in order) moved the arg-3 rejection
after the up-front charge: `string.gsub` now charges
`(len(s) + len(pattern)).max(1)` plus the pattern bytes, then raises
`StringFunctionOrTableExpected` for a missing or wrong-typed argument 3. A
failing call's `cost_used` changed, and under a tight budget the reported error
can be budget exhaustion instead of the argument error. Other string functions
validate every argument before charging. Moving the `repl_type` check ahead of
the charge (it reads no pattern state) restores validate-then-charge.
