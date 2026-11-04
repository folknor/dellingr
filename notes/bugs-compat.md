# Compatibility findings: divergences missing from docs/compatibility.md

Filed from the 2026-11-04 defect hunt (scopes `compiler-front`, `vm-dispatch`,
`stdlib-strings`, `stdlib-core`). Each entry is a behaviour that differs from
lua5.2 or lua5.4 and is not listed on the page that says "Every deliberate
divergence from Lua 5.2 and 5.4 ... A difference not listed here is a bug."
Resolving one means adding it to the page, or changing the code if the
divergence was not deliberate. Hunt reports are work documents and may be wrong;
verify before fixing.

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

## COMPAT-001 - No proper tail calls, and the divergence is listed nowhere

Scope: `vm-dispatch`. Hunter's label: medium. Replay-visible either way: it
changes which scripts succeed.

Claims broken: `docs/compatibility.md`'s opening sentence; neither that page nor
the README's "Won't implement" list mentions tail calls.

The instruction set has no tail-call opcode (`instr.rs` `opcode_rows!` ends at
`OP_CALL`, `OP_RETURN`, `OP_VARARG`, `OP_CLOSURE`). `return f(x)` compiles to
`OP_CALL ... RetCount::All` followed by `OP_RETURN All`. The caller's activation
record and frame stay live for the whole callee (`State::admit_lua_call` pushes
a `CallInfo`; `Frame::eval_batched` pushes a frame on `Leave::Call`), and every
level counts against `MAX_CALL_DEPTH` (1000). Both references guarantee proper
tail calls, so tail recursion runs at constant depth there.

Effect on script authors: idiomatic tail-recursive code works in lua5.2 and
lua5.4 and dies here with `CallDepthExceeded` after 1000 levels. Examples:
state machines (`return state_b(ctx)`), CPS loops, and
`local function loop(n) if n == 0 then return end return loop(n - 1) end
loop(5000)`. Errors kill the callback, so this is a hard failure. Second
effect: a `__call` chain charge held in a tail-calling activation's record
(`CallInfo::chain_charges`) stays held until the whole tail chain unwinds, so
callable-object tail recursion also hits `MAX_METAMETHOD_DEPTH` (200).
Tracebacks also differ, since Lua prints `(...tail calls...)`.

Direction the hunter gave: the dispatch loop already has what an `OP_TAILCALL`
needs, since frames live on the invocation's own `Vec<Frame>` and admission is
separate from execution. A tail call would close the frame's upvalues, slide
the callee and its arguments down into the current function slot, pop the
current activation (carrying its chain charges into the new record or releasing
them), admit the callee in place, and replace the top frame instead of pushing
one. `pending_call_results` keeps working because the caller's suspended
`OP_CALL` still decides the result count; a Rust callee in tail position can
stay an ordinary call followed by a return. If tail calls stay unsupported on
purpose, they belong in the README's "Won't implement" list and in
`compatibility.md`, with the depth consequence. The hunter notes that merging
`CallInfo` into `Frame` (tracked in `notes/optimizations.md`, call path after
the frame-stack flattening) would make the in-place tail call simpler.

## COMPAT-002 - The modulo operator follows Lua 5.4's fmod definition, unlisted

Raised by `compiler-front` (fuller account) and `vm-dispatch`. Changing the
semantics would be replay-visible and would also change folded literals.

`numeral::lua_modulo`, shared by the VM and the constant folder, computes
`fmod(a, b)` and then corrects the sign: Lua 5.4's `luai_nummod`. Lua 5.2
defines `a % b` as `a - floor(a/b)*b`. They differ visibly for ordinary inputs:

- `1 % 0.1`: 5.2 gives `0` (because `1/0.1` rounds to exactly 10); 5.4 and
  dellingr give `0.1` (`0.09999999999999995`).
- `5 % math.huge` (and `1 % math.huge`): 5.2 gives `nan`; 5.4 and dellingr give
  the dividend. `-5 % math.huge` (and `-1 % math.huge`): 5.2 `nan`, dellingr
  `inf`.

Where the page picks 5.4 over 5.2 elsewhere it says so (`gsub` negative `n`,
integer arguments, `error` prefixes); it says nothing about `%`. Fix: a line
under "Numbers", unless the 5.2 definition was intended.

## COMPAT-003 - goto is an ordinary identifier, and attempts to use it get misleading errors

Scope: `compiler-front`. Hunter's label: medium-low.

`token.rs` `KEYWORDS` has no `goto`, so the lexer returns it as `Identifier`:

- `local goto = 1`, `goto = 5` and `goto` used as a variable all compile. Lua
  5.2 and 5.4 reject them because `goto` is reserved in both. This
  accepted-program divergence is not listed. The README's "Won't implement:
  `goto`/labels" says the feature is absent, not that the name becomes free.
- `goto continue` gives `syntax error near 'continue'` (the parser reads `goto`
  as a global-variable expression statement), and `::continue::` gives
  `unexpected symbol near ':'`. Neither says `goto` is unsupported. Compare
  `_ENV` (`SyntaxError::UnsupportedEnvironmentName`) and long strings
  (`LongStringUnsupported`), which have dedicated "not supported" errors for the
  same reason.

Claims broken: the page's opening sentence, and the project's rule that a
misleading message is a defect for the script author. Fix options: reserve
`goto` with an "unsupported" error as for `_ENV` and long strings, or document
that the name is free.

## COMPAT-004 - Unlisted 5.2/5.4 picks in the lexer, parser and numeral code

Scope: `compiler-front`. Each is a deliberate choice of one reference, recorded
only in a test comment or the CHANGELOG.

- A numeral glued to a letter (`3or 4`, `1e5or`) is rejected (`lexer.rs`
  `lex_exponent` and the hex branch, `is_identifier_continue`). Lua 5.2 accepts
  `3or 4`. Recorded only in the `numeral_identifier_adjacency_is_malformed` test
  comment and CHANGELOG ("tracking 5.4").
- `(` on a new line continues a call (`f\n(g)`; `parse_prefix_suffix`). Lua 5.2
  rejects it with "ambiguous syntax (function call x new statement)". The test
  `a_new_line_paren_continues_the_call` says "as in reference Lua", true only
  for 5.4.
- Hex numerals without `.`/`p` do not wrap: `0x10000000000000000` is
  `1.8446744073709552e19`, and `0xffffffffffffffff` is about `1.8e19`. In Lua
  5.4 they wrap to `0` and `-1`. The same holds for
  `tonumber("0xffffffffffffffff")` through `parse_lua_numeral`. The page lists
  the wrap difference for `tonumber(s, base)` only. "Numbers are f64 only"
  arguably implies it, but the analogous case is spelled out and this one is
  not.
- CLI lateral (`src/main.rs`): `lua script.lua` skips a first line starting with
  `#` (a shebang), but `dellingr script.lua` reports `invalid character '#'` or
  `unexpected symbol`. `docs/cli.md` does not mention it.

## COMPAT-005 - Unlisted numeric divergences: n % 0 and numeric for past 2^53

Scope: `vm-dispatch`.

- `n % 0` with whole-number operands is `nan` here; Lua 5.4 raises "attempt to
  perform 'n%%0'". Arguably implied by "Numbers are f64 only", but not stated.
- A numeric `for` whose control crosses 2^53 never terminates here, because
  `var += step` stops changing (for example `for i = 2^53 - 1, 2^53 + 1 do`).
  Lua 5.4's integer loop runs exactly three times; Lua 5.2 also hangs. The page
  mentions the 2^53 effect only for table keys.

## COMPAT-006 - The string library's version choices are not on the page

Scope: `stdlib-strings`.

The page already records some per-version string choices (`gsub` negative `n`
"as in Lua 5.4", `gmatch`'s 5.4 `init`), so it intends to record where the
hybrid picks one reference. Missing, though recorded in code comments or the
`src/lua_std.rs` module doc (which is not the contract):

- Lua 5.4's empty-match rule in `gsub` and `gmatch` (`gsub_lua_pattern`,
  `gmatch_iter`). `("abc"):gsub("%a*", "-")` is `"-", 1` here and in 5.4, but
  `"--", 2` in 5.2. `gmatch("abc", "%a*")` yields only `"abc"`, where 5.2 also
  yields `""`. This changes results scripts can see.
- `string.format` follows 5.4: `%p` exists (5.2: "invalid option"); `%q` accepts
  numbers, nil and booleans and renders them as literals (5.2 quotes a number as
  a string, `"1"`, and rejects nil and booleans); flag and precision combinations
  are validated per conversion ("invalid conversion specification"), where 5.2
  hands them to `sprintf`. Only the `src/lua_std.rs` module doc and comments in
  `string_format.rs` (`classify_conversion`) mention `%p`.
- In a `gsub` replacement string, `%` followed by a byte that is neither a digit
  nor `%` raises "invalid use of '%' in replacement string" (5.4); 5.2 inserts
  the byte literally (`compile_replacement_template`,
  `ReplSegment::InvalidEscape`).

The corpus accepts a match with either reference ("dellingr is a Lua 5.2/5.4
hybrid", `run_examples.rs`), so no test enforces the page here. Fix: add a
"String library" section listing these choices, and point the `lua_std.rs`
module doc at it instead of duplicating.

## COMPAT-007 - Unlisted base and table library divergences

Scope: `stdlib-core`. None gives a wrong result; each raises an error or skips a
hook where a reference behaves differently.

- `select` treats only the exact string `"#"` as the count form. lua5.2 and
  lua5.4 test only the first byte (`*lua_tostring(L, 1) == '#'`), so
  `select("#n", ...)` returns the count there and raises "number expected, got
  string" here.
- `ipairs` ignores `__ipairs`, which Lua 5.2 honours. The README's "ignored
  fields" list and the page name only `__pairs`.
- `ipairs(v)` requires a table (`check_type_named(1, Table, "ipairs")`). Lua 5.4
  takes any value (`luaL_checkany`) and indexes it through `__index`, so
  `ipairs("abc")` iterates zero times there. Lua 5.2 agrees with dellingr.
- `table.sort` never raises "invalid order function for sorting"; the heap sort
  quietly accepts an inconsistent comparator. Stated only in a code comment in
  `table_sort` (`vm/table_ops.rs`). The page covers tie order and handler call
  order, not this.
- `table.sort` type-checks a non-nil second argument even when the length is 0
  or 1. Lua 5.4 checks the comparator only when `n > 1`, so
  `table.sort({}, 42)` succeeds there and raises here. Listed only for
  completeness.
