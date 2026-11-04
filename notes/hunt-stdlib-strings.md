# Hunt: stdlib-strings

Scope: `src/lua_std/string.rs`, `src/lua_std/string_format.rs`,
`src/patterns/` (`mod.rs`, `luapat.rs`, `errors.rs`), `src/gmatch_schema.rs`,
plus the boundaries they cross (pattern cache and gmatch matcher in `vm.rs`,
`tostring_bytes_unguarded` / `format_pointer_id` in `vm/table_ops.rs`,
`push_bytes_from_stack_range`, the gmatch snapshot verifier at a glance).

Summary: the matcher and the library functions held up under a close
read. I checked the pattern engine item by item against reference
`lstrlib.c` (`match`, `max_expand`, `min_expand`, `matchbalance`,
`match_capture`/`check_capture`, `classEnd`, `matchbracketclass`,
`start_capture`/`end_capture`, the `$` rule, the frontier at the subject
ends). I also checked the gsub/gmatch empty-match rule against 5.4's
`lastmatch` loops, the position rules of `sub`/`find`/`match`/`gmatch`
against `posrelatI`/`getendpos`, the `gsub` replacement template against
5.4 `add_s`/`get_onecapture`, and the `string.format` flag/precision
tables against 5.4's `L_FMTFLAGS*` and `checkformat`. I found no
wrong-result defect in any of these. The findings are doc/contract gaps
and one interrupt-latency claim the code exceeds. Structural and
performance opportunities follow, labelled as such.

## Findings (defects)

### F1. `docs/compatibility.md` omits the string library's version choices

Claim broken: `docs/compatibility.md` opens with "Every deliberate divergence
from Lua 5.2 and 5.4. A difference not listed here is a bug." The doc does
list some per-version string choices (`gsub` negative `n` "as in Lua 5.4",
`gmatch`'s 5.4 `init`), so it plainly means to record where the hybrid picks
one reference. Missing from it, though recorded in code comments or the
`src/lua_std.rs` module doc, which is not the contract:

- **Lua 5.4's empty-match rule in `gsub` and `gmatch`** (`gsub_lua_pattern`,
  `gmatch_iter`). `("abc"):gsub("%a*", "-")` is `"-", 1` here and in 5.4, but
  `"--", 2` in 5.2. `gmatch("abc", "%a*")` yields only `"abc"` where 5.2 also
  yields `""`. This one changes results that scripts can see. Only
  `src/lua_std.rs`'s module doc and comments in `string.rs` mention it.
- **`string.format` follows 5.4**: `%p` exists (5.2: "invalid option"). `%q`
  accepts numbers, nil and booleans and renders them as literals (5.2 quotes a
  number as a string, `"1"`, and rejects nil/booleans). Flag/precision
  combinations are validated per conversion ("invalid conversion
  specification"), where 5.2 hands them to `sprintf`. Only the module doc
  of `src/lua_std.rs` and comments in `string_format.rs` (`classify_conversion`)
  mention `%p`.
- **`gsub` replacement string: `%` followed by a non-digit, non-`%` byte
  raises** "invalid use of '%' in replacement string" (5.4). 5.2 inserts the
  byte literally. `compile_replacement_template` / `ReplSegment::InvalidEscape`.

The corpus accepts a match with either reference ("dellingr is a Lua 5.2/5.4
hybrid", `run_examples.rs`), so no test enforces this doc. Fix: add a "String
library" section to `compatibility.md` listing these choices (and point the
`lua_std.rs` module doc at it rather than duplicating it).

### F2. `docs/budget.md` under-describes the string-library charge units

Claim broken: `docs/budget.md` "Native charges / String library" is the
listing of the units each site measures ("A charge site measures its work in
the units listed above"), and "Flat charges are deliberate" makes those units
part of the cost model. The code charges units the page does not list:

- **Returned capture bytes.** `find` (Lua-pattern path), `match` and every
  `gmatch` iteration charge the byte length of the captures they push
  (`preflight_capture_bytes`). `gsub` charges the capture bytes it passes to a
  replacement function, or as the table key. The page lists none of this.
- **`gsub` output.** Unmatched subject segments copied to the result are
  charged per byte (`append_bytes` for `s[pos..start]` and the tail), and so
  is the whole match when a function/table replacement keeps the original.
  The page says only "gsub also charges replacement text bytes".
- **The template is charged twice per application.** Each application of a
  string replacement charges `repl.len()` up front (`ReplTemplate::repl_len`,
  and the first application's `state.charge(.., repl.len())`). It then charges
  every literal byte and capture byte it emits. A literal template `"xyz"`
  costs 6 per match, not 3. The code comment at the top of `string.rs` names
  the pattern-bytes double charge but not this one, and so does the page.
- **`format`'s `%s` charges the rendering of any argument**, numbers and
  `__tostring` results included, before precision truncation. The page says
  "bytes of string arguments consumed".

None of these needs a cost change. The page should state the units the code
already charges (`COST_MODEL_VERSION` is untouched by a doc fix).

### F3. `string.format` can do ~254 full 16 MiB passes between two safepoints

Claim broken: `src/vm/interrupt.rs` ("Between two safepoints the work is at
most one linear pass over a string capped by `MAX_STRING_BYTES` or over a
table's storage") and `docs/budget.md` ("Stopping a script from the host",
same sentence). The `format` loop's own comment ("one pass over the format
string's bytes, each directive rendering one argument") states the per-directive
bound but not the total.

`string.format` has no safepoint of its own. A directive polls only when it
runs a `__tostring` handler. Per argument directive it does one full pass over
the argument:

- `%s`: `tostring_bytes_unguarded` copies the whole rendering (`to_vec()`).
  With a precision, `bytes.truncate` drops it afterwards. So `%.0s` copies
  16 MiB and appends nothing, and the output cap
  (`checked_string_growth`) never bounds it.
- `%q` of a string: `to_vec()` plus `quote_string`'s pass.
- `%d`/`%f`/... of a numeral string: `to_vec()` plus `parse_lua_numeral`
  (a valid numeral can be 16 MiB of whitespace around one digit).

With `MAX_ARGUMENTS` = 255, one call can make 254 such passes over 16 MiB
arguments, about 4 GiB of copying with no poll. Under a default model the
charge (16M per `%s`) usually stops this through the budget. But the
interrupt contract holds whatever the costing: `Costing::Off`, a zero
`StringFormat` weight (the `budget.md` example itself sets one) and an unset
budget all reach it. The work is bounded, so it adds interrupt latency, not a
hang.

Fix: poll once per directive in the `format` loop. That restores the stated
bound and costs nothing, since interrupts charge nothing. Separately, render
`%s` with a precision by slicing before copying. Charge the full length as
today ("Charge the full rendering before precision truncation" stays true),
but copy only `min(precision, len)` bytes. For a plain string with no
`__tostring`, borrow the stack value instead of `to_vec()`.

### F4. A lint is disabled in scope

Claim broken: AGENTS.md "Never disable a lint; fix the code."
`saturating_usize` in `string.rs` carries `#[allow(clippy::disallowed_methods)]`
for `Result::unwrap_or`. The `clippy.toml` reason text for that ban does say
"or use a scoped allow for an intentional fallback", so the repo's two rule
sources disagree. One of them should change. The code fix is trivial
(`usize::try_from(n.max(0)).map_or(usize::MAX, |v| v)`, or `match`). The same
pattern exists out of scope in `lua_std/basic.rs`, `vm/stack.rs`,
`crmath/mod.rs`, `host.rs` and `main.rs`.

### F5. Doc nit: the empty-pattern `gsub` is filed under the wrong safepoint class

`src/vm/interrupt.rs` lists the empty-pattern `string.gsub` among "the library
loops whose trip count a script chooses by a number". Its trip count is the
subject length plus one, not a numeric argument. The poll in
`gsub_empty_pattern` is right. Only the classification is wrong. Reword it
as "each position of the empty-pattern `string.gsub`" or similar.
`docs/budget.md` lists it neutrally and is fine.

## Checked and found correct (so nobody re-hunts them)

- Lazy pattern errors. Each `Item::Fail` sits where reference raises. The
  compile-time backreference check equals `check_capture`, because items are
  strictly linear. `TooManyCaptures` counts position captures. A match cannot
  succeed past a `Fail`, because `EndAnchor` is compiled only as the final
  byte.
- Depth accounting matches `matchdepth`: `()` recurses, `*`/`-` that fail
  once take the depth-free `goto init`, and `?` recurses once.
- `CaptureEnd` reset on failure. Fresh `MatchState` per start position, so no
  stale captures leak into `captures()`.
- `gsub` search-based loop equals 5.4's match-at-`src` loop, including anchor,
  `lastmatch` rejection only at `pos`, tail copy, and the replacement-limit
  stop. Termination is guaranteed because a rejected empty match always
  advances.
- `gmatch_iter` retry runs at most twice. Commit staging leaves `pos`
  unchanged on any refused push or interrupted match. The compile-charged flag
  is committed together with its charge.
- `find`'s plain classification (`)` literal) equals 5.4 `nospecials`.
  `gsub`'s `is_plain` is conservative and exact (unit test covers all bytes).
- Template `%0`, `%1` with no captures, out-of-range `%N`, unfinished
  captures raised only when read (template) or always (function/table), as in
  5.4.
- `string.format`: flag tables per conversion, `#o` precision rule, `#x` with
  0, zero padding after `0x` for `%a`, no zero padding for inf/nan, `%g`
  P/X rule and trailing-zero strip, `%c` truncation, `%q` NaN/inf/-0.0/i64
  range, the `NoValue` before invalid-conversion ordering for a trailing `%`.
- Pattern cache: keyed by interned pointer plus effective anchor mode, pruned
  after sweep, invisible to cost. The gmatch matcher reads the subject only
  while nothing allocates.

## Structural and performance opportunities (not defects)

### S1. Replace the gmatch Lua-table state and Lua shim with a native iterator object

The gmatch iterator is a Lua closure compiled from source
(`GMATCH_WRAPPER_SRC`), because a `RustFunc` cannot carry upvalues. Its state
is a private Lua table with string keys (`s`, `p`, `pos`, `lastmatch`, a
`0 -> "compile_charged"` indirection, `len`). Everything else follows from
that:

- per iteration: about six `push_string` calls (intern = hash + lookup),
  four raw table reads, full type/range revalidation (`gmatch_state_index`,
  `GmatchStateFault`), and a `Vec<LuaCapture>` allocation;
- `is_gmatch_wrapper` identity matching and frame omission in error reporting
  (`reference/error-reporting.md`);
- `gmatch_iterator_bytecode` substitution on snapshot load, and the
  verifier's gmatch section (`validate_gmatch_*`), which re-derives
  invariants the runtime then re-checks.

A typed heap object (subject `StringPtr`, pattern `StringPtr`, `pos`,
`lastmatch: Option`, `compile_charged`), or native closures with typed
upvalues, would delete the shim, the identity hack, the field interning and
most of the fault enum. It would make the iterator a plain `RustFn` call and
give snapshots one typed record. The rewrite changes the snapshot format and
the error-frame rule. It changes no cost and no `pairs` order, since the
table is unreachable from scripts.

### S2. Subject copies per call

`string_or_number_bytes` copies the subject and pattern on every
`find`/`match`/`gsub`/`gmatch` call. The doc comment calls this deliberate
and names the fix: shared ownership of heap string bytes (an `Rc<[u8]>`-style
handle). With that handle, `find`, `match`, `gsub` and `format` (`%s`, `%q`,
numeral args) would be borrow-only, and F3's copying would disappear with it.

### S3. gmatch construction re-interns what is already on the stack

`gmatch` copies `s`/`pattern` into `Vec`s, then `push_bytes(&s)` re-interns
them, hashing up to 16 MiB to find the string that is already at stack
slot 1/2. When the argument is a string, push the existing value
(`push_value(1)`) instead.

### S4. `MatchState::new` per start position

`str_match` builds a fresh `MatchState` for every start position, which
initialises the 32-entry capture array (about 512 bytes). A failed search over
an n-byte subject does n of these. Every path writes a capture slot before
reading it, and `captures()` reads only slots the successful path set. So
the state can be built once per search and reused, keeping
"fresh per search" (the `SharedPattern` doc) intact.

### S5. `string.format` appends literal bytes one at a time

Each literal byte of the format string goes through `append_output`
(`checked_string_growth` plus an atomic `state.charge` of 1). Batch literal
runs with `charge_units`, which matches per-byte admission exactly (the same
technique as `append_bytes_units` in gsub). `%%` also goes through
`parse_directive` + `format_argument` + a `Vec` allocation to emit one byte.

### S6. Cost-design review item: the double-charged template

F2 asks only for documentation. As a design question: the template-length
charge per application plus per-byte emission charges a literal template
twice, while a function replacement returning the same text is charged once.
If that asymmetry is unintended, removing it is a `COST_MODEL_VERSION` bump.
