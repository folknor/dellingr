# Defect hunt: strings (string library, string.format, pattern engine)

Scope: `src/lua_std/string.rs`, `src/lua_std/string_format.rs`,
`src/patterns/{mod,luapat,errors}.rs`, `src/gmatch_schema.rs`, plus what they
lean on (`State::cached_pattern_matcher` / `with_gmatch_matcher`, the GC's
pattern-cache clear, `tostring_rendering`, `format_pointer_id`, the lexer's
string escapes for `%q` round-tripping).

Read-only review; nothing was built or run. Every finding below was traced
through the code, but none was reproduced.

Overall: the pattern engine holds up well when checked step by step against
5.4 `lstrlib.c`. That covered class end and bracket-class arm order, the
frontier, balance, backrefs, the depth accounting (`MAXCCALLS`, including `()`
recursing), lazy `Item::Fail` placement, unclosed-capture reporting, the
anchor modes, and the gsub/gmatch `lastmatch` rule. `string.format` matches
5.4's per-conversion flag and precision tables, its two-digit field limits,
and C's `%#o`, `%#x`, `%g`, `%a` and zero-padding rules. I found no
wrong-result defect in either. The findings are a partial-failure tear in
the gmatch iterator, one misleading error location, one argument-order
inconsistency, and stale documentation.

---

## 1. gmatch iterator state can tear or skip a match on a refused stack push (medium)

`gmatch_iter` (string.rs) writes its persistent iterator state before it has
pushed what it returns, and nothing reserves the stack first:

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

- **Torn state.** If the `LAST_MATCH_FIELD` push or its value push is
  refused, `pos` has advanced but `lastmatch` still holds its old value (or
  nil). On the next call, an empty match at the new `pos` is not rejected,
  because `lastmatch != pos`. So the iterator yields an extra `""` that the
  5.4 empty-match rule forbids. Example: `("abc"):gmatch("%a*")` yields
  `"abc"`, and then `""` instead of ending. The lua_std.rs header and the
  string.rs comments both promise this rule.
- **Skipped match.** If a capture push is refused after both fields are
  written, the match is consumed but never returned. The next call resumes
  past it.

The State survives an error and can be reused. A gmatch closure stored in a
global is reachable from the next callback, and a snapshot taken afterwards
saves the bad state, which the verifier accepts. So this is a real
continue-after-error hazard. The project's own pattern elsewhere is to
preflight before mutating. Even `preflight_capture_bytes` is deliberately
ordered before the writes "so errors leave nothing half-done". But cost is
the only thing it preflights, not stack space.

The `fault-injection` sweep checks surviving operands and State reuse. It
does not re-drive the same iterator after an injected refusal, so this
escapes it.

**Fix:** `check_stack_space(2 + returned.len())` before the first
`set_table_raw`. Better still, restructure:
- `lastmatch` is redundant. After any accepted match it always equals `pos`,
  and `pos` is never moved on a miss. Store one `pos` plus a "has matched"
  boolean (or encode "no match yet" as `lastmatch = nil` and drop the
  separate `pos` write). Then there is a single table write, and nothing can
  tear.
- Push the captures first and write the field last, so a refused push leaves
  the iterator untouched.

The snapshot schema (`gmatch_schema.rs`, `save_state/verify.rs`) would
shrink with it.

## 2. gmatch errors are located in the synthetic wrapper chunk, not the script (low)

Every other string function's runtime error, such as a malformed pattern,
budget exhaustion or an unfinished capture, is located at the script line
that called it. That happens through `locate_in_frame` on the innermost Lua
frame.

gmatch is different. Its iterator is a RustFunc called from the
`GMATCH_WRAPPER` Lua closure (`return iter(state)`). dellingr has no tail
calls, so that wrapper frame is innermost when `gmatch_iter` fails. The error
therefore gets `string.gmatch` line 3 as its location. For
`for w in s:gmatch("(") do`, the headline location names a chunk the script
author never wrote. Their line appears only further down the traceback.

This breaks the location contract `reference/error-reporting.md` opens with
("Errors carry a structured `ErrorKind`, source location..."). It also makes
gmatch inconsistent with `find`/`match`/`gsub` raising the identical error.
The code comment says the chunk name exists so errors "stay identifiable in
traces", which accounts for the traceback entry but not for taking over the
primary location.

No test pins the `string.gmatch:` prefix. That suggests nobody has decided
the current behaviour is the intended one.

**Fix:** either locate errors raised through the wrapper at the caller's
frame, or remove the wrapper entirely. A RustFunc that can carry an upvalue
or a bound argument would remove the process-global chunk, the
`call_unguarded` native level in `push_gmatch_wrapper`, the snapshot bytecode
for the wrapper, and this location quirk all at once. That is a structural
improvement worth considering on its own merits.

## 3. `string.gsub` reports a missing argument 3 before checking arguments 1 and 2 (low)

`gsub` calls `check_any_named(state, 3, "gsub")` first, before arguments 1
and 2 are type-checked. In string.rs, the doc comment on
`check_string_or_number` states that "an argument must be validated before
later arguments are read, so errors name the arguments in the reference's
order". `sub` follows that rule explicitly. `gsub` breaks it:

- `string.gsub("x", {})` raises `bad argument #3 to 'gsub' (value expected)`.
  The actual first problem is argument 2, a table where a pattern was
  expected. Reference 5.4 reports `#2 ... string expected, got table`.
- `string.gsub()` raises the `#3` error, where reference reports `#1`.

The message points the script author at the wrong argument, so it is not
merely a wording difference. The `check_any_named(3)` call is also
redundant: the `repl_type` check further down already rejects `None`/absent
with `StringFunctionOrTableExpected`, which is the reference's own message.

**Fix:** drop the call.

## 4. Misplaced doc comment: the `pattern_matcher` doc sits on `gsub_empty_pattern` (low, doc)

In string.rs, the block that begins "The matcher for argument 2 of `find`,
`match` or `gsub`. A string pattern comes from the State's compiled-pattern
cache; a number coerced to `pattern` has no interned key and compiles
fresh..." sits directly above `#[inline(never)] fn gsub_empty_pattern`. So
rustdoc and IDE hovers describe `gsub_empty_pattern` as the pattern-matcher
helper. The function it actually describes, `fn pattern_matcher` further
down the file, has no doc comment.

**Fix:** move the comment.

## 5. AGENTS.md names string.rs helpers that do not exist (low, doc)

AGENTS.md, "Patterns" paragraph: "`string.rs` has helpers
`is_plain_lua_pattern` (fast path) and `gsub_replacement`." Neither exists:

- The plain-pattern classifiers are `crate::patterns::is_plain` (gsub) and
  `is_plain_for_find` (find, which treats `)` as literal), in
  `src/patterns/mod.rs`.
- The replacement dispatcher is `append_gsub_replacement`, with
  `append_templated_replacement` / `append_function_replacement` and the
  `ReplTemplate` compiler.

`notes/optimizations.md` also still refers to `is_plain_lua_pattern`. Per the
documentation rule, reword AGENTS.md to describe the roles rather than
re-pinning names that drift.

---

## Lateral observations (not defects against a stated claim)

- **Dead check in `bracket_class`** (luapat.rs): `if class_start == close`
  can never be true. The scan loop always advances `scan` past `class_start`
  before it can break with `close = scan`. The `[]` / `[^]` cases it seems to
  guard are already rejected by the loop running off the end. This is
  harmless, but the comment-free check suggests a contract it does not
  enforce.
- **Pattern compile cost:** uncharged pattern compilation is
  `O(pattern bytes x 256)`, because every `%x` escape, inside or outside a
  bracket, loops over all 256 bytes. Up to `MAX_STRING_BYTES` of pattern can
  therefore cost about 256x its byte charge in host work. This repeats on
  every call when the cache misses: every GC clears the whole pattern cache,
  and a number-typed pattern always bypasses the cache. The `class_for_escape`
  comment documents this, and it fits the "structural work is free" framing,
  so it is a performance note only. `match_class` results per escape letter
  could be a 22-entry static table of `ByteClass`es, which makes each escape
  a copy.
- **Pattern cache cleared wholesale on every collection**
  (`collect_garbage`): this is correct, since keys are string pointers and
  freed slots can be reused. But under the default GC policy (threshold
  20 allocations) a pattern-heavy loop recompiles constantly. Two
  alternatives would keep entries whose key string survived the sweep:
  - key the cache by content hash plus bytes, or
  - drop only dead keys after the sweep, the way `format_pointer_ids` is
    pruned with `val_is_live`.
- **`gmatch_iter` per-call overhead:** each call re-interns four or five
  field-name strings through `push_string`, which may run `maybe_collect` and
  so clear the pattern cache, and does about six raw table reads and writes.
  Folding finding 1's redundant `lastmatch` field away also removes a write
  per iteration.
- **`string.format` `%s` charges twice:** `%s` charges its rendering in
  `format_argument` and charges the padded result again in `append_output`.
  budget.md describes both ("bytes appended to the output and bytes of string
  arguments consumed"), so this is consistent. It is just worth knowing that
  `%s` costs roughly 2x per byte.
- **`InvalidGsubCaptureIndex` drops the index:** it displays as
  `invalid capture index` without the `%N` it carries in its typed field.
  The pattern-side `InvalidCaptureIndex` renders `%N`. The two errors read
  differently for the same mistake. This is not a defect under the wording
  rule, but it is an easy consistency win.
