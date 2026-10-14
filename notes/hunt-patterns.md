# Hunt: patterns (`src/patterns/`)

Measured against reference `lstrlib.c` (5.2 and 5.4); reference behaviour below
was confirmed with `lua5.4 -e` / `lua5.2 -e` probes on this host.

Overall: the matcher core (`match_at`, `max_expand`, `min_expand`, balance,
frontier, backrefs, bracket-class branch order, depth accounting, `$` handling,
quantifier attachment, position captures, capture limit) agrees with reference
in every case I traced. The real defect is in *when* errors are raised.

## 1. Eager compilation raises errors reference never raises (defect)

Claim broken: "output matching reference Lua for everything it does
implement" (no `-- DIFF:` marks this, and nothing in README/reference documents
it). Because errors kill the callback, this turns a working reference script
into a dead one.

Reference validates a pattern lazily: `match` only inspects the pattern text
it actually reaches (`classEnd`, `check_capture`, `capture_to_close`,
`start_capture`'s level check, `%b`/`%f` argument checks all run at match
time). `Compiler::compile` in `luapat.rs` validates the whole pattern up front,
and `string.find`/`match`/`gsub`/`gmatch` all call `from_bytes_try` before
matching. So any malformed tail behind a prefix that never matches errors in
dellingr and returns nil in reference:

| call | lua5.4 / lua5.2 | dellingr |
|---|---|---|
| `string.match('b', 'a)')` | `nil` | error "invalid pattern capture" |
| `string.gsub('b', 'a)', '')` | `b 0` | error |
| `string.find('abc', 'x(')` | `nil` | error "unfinished capture" |
| `string.find('abc', 'x%1')` | `nil` | error "invalid capture index %1" |
| `string.find('abc', 'x[')` | `nil` | error "missing ']'" |
| `string.match('b', 'a' .. ('()'):rep(40))` | `nil` | error "too many captures" |

(`'x%1'` and `'x['` are not just "error vs nil at different times": reference
never errors on them for any subject that lacks an `x`.)

Errors that reference raises *on success* (e.g. `'('` matching, then
`get_onecapture` raising "unfinished capture") are reproduced, since eager
validation is a superset. The divergence is strictly one-directional: dellingr
errors in more cases.

Right move: either (a) keep the compiled program but make validation lazy by
compiling an error into the item stream (`Item::Fail(PatternError)` at the
first malformed point, raised when `match_at` reaches it - this keeps the
compile-once structure and costs nothing on valid patterns), or (b) declare
eager validation a deliberate divergence in README and add a `-- DIFF:`
example. (a) is cheap and restores reference parity exactly, because every
reference error site corresponds to a fixed pattern position. Note the
"unfinished capture" case needs care: reference raises it only when pushing
results of a *successful* match, so an unclosed `(` must become a deferred
"fail on success" flag, not an `Item::Fail` at the `(`.

## 2. gsub/gmatch empty-match semantics are 5.2, while other surfaces are 5.4 (lateral, in `string.rs`)

`gsub` and `gmatch_iter` advance by one byte after an empty match
(`new_pos = end + (start == end)`), which is 5.2 behaviour. 5.4 uses
`lastmatch` to reject an empty match at the end of the previous match:
`('abc'):gsub('%a*', '-')` is `-- 2` in 5.2 and `- 1` in 5.4; `gmatch('%a*')`
yields `abc`, `` in 5.2 and only `abc` in 5.4. The diff harness accepts either
reference, so this passes per-file, but other surfaces are explicitly 5.4
(`string_format.rs` is "the Lua 5.4 `string.format` engine", gmatch escapes a
leading `^` as 5.4 does). A script exercising both can match neither
reference. No document states which version gsub/gmatch follow. Pick one
(5.4 would be consistent with the rest) and say so.

## 3. Smaller findings

- `Item::Backref`'s `CapLen::Unfinished` arm returns
  `PatternError::UnfinishedCapture`; reference raises "invalid capture index
  %N" at that site (`check_capture`). The arm is unreachable today because the
  compiler rejects backrefs to open captures, so it is dead code carrying a
  wrong message. If finding 1 is fixed by moving validation back to match time,
  this arm becomes reachable and the message must change.
- `PatternError::InvalidCaptureIndex(None)` is never constructed by the
  pattern engine (only by tests in `errors.rs`); the `Option` exists only for
  the shared `PatternErrorDetail` used by gsub replacement errors. Smell, not a
  defect.
- `max_expand` takes a `min` parameter that every caller passes as `0` (`+`
  pre-advances `pos` instead). Dead generality; the `end == pos + min` break is
  just `end == pos`.
- `MatchState::captures` can return `UnfinishedCapture` on a successful match,
  but success traverses every item (no alternation, `$` only at the end), and
  the compiler rejects unclosed captures, so that path is unreachable. Fine as
  a guard; worth an `expect`-style comment rather than a user-facing error.
- Compile-time host work is not proportional to the charged cost: each `%x`
  escape and each bracket-class escape loops over all 256 bytes, and each
  class is interned through a `BTreeMap<[u64;4]>`. Callers charge
  `pattern.len()`, so a pattern of N escapes does ~256N uncharged work. This is
  within the README's "structural work is free" framing and bounded, so not a
  cost-model defect, but a static 256-bit table per class letter (computed
  once, `const`) would remove the loop entirely.
- The empty-item fast path in `str_match` (pattern `""` or `"^"`) charges no
  matcher cost, whereas every other match charges at least one `charge()`.
  Deterministic, harmless, but an inconsistency in "matcher primitives each
  consume budget".

## Checked and found consistent with reference

Bracket class scanning (`[]]`, `[^]]`, `[%]]`, `[]`/`[^]` errors), branch
order inside classes (`[z-a]`, `[%a-z]`, `[%--a]`), `match_class` including
uppercase non-class letters and `%z` (still accepted by 5.4), `$` only as the
final byte, quantifiers never attaching to `)`, `%b`, `%f`, backrefs or `()`,
`%b` close-before-open check, frontier at subject start/end using `\0`,
backref to a position capture failing, `MAXCCALLS` = 200 active frames with
`(`, `)`, `()`, `?`, `+`/`*`/`-` expansions recursing and the empty-accept
transitions not recursing, per-start-position reset of depth and captures,
anchored single attempt, capture limit 32 counting position captures,
`is_plain_for_find` treating `)` as literal (reference `SPECIALS` lacks `)`),
and `gmatch` turning a leading `^` into a literal.
