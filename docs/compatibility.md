# Compatibility with reference Lua

Every deliberate divergence from Lua 5.2 and 5.4. A difference not listed
here is a bug. Features that are absent altogether are listed in the README
under "Won't implement".

Reference outputs are compared by `scripts/diff_test.sh`; intentional
differences carry a `-- DIFF:` comment in the example.

## Numbers

- Numbers are f64 only. There is no integer subtype.
- `tostring` and `string.format` render numbers with Lua 5.2's `%.14g`
  (`0.3`, `1e+15`, `-0`). Whole-valued floats print without Lua 5.4's `.0`.
- NaN prints as `nan` in `tostring` and `string.format`, never `-nan`.
  C Lua on x86-64 prints `-nan` for `0/0`. `%q` prints `(0/0)`.
- `tonumber(s, base)` accumulates in floating point, as Lua 5.2 does. Lua 5.4
  wraps to a 64-bit integer.
- `math.sin`, `cos`, `tan`, `asin`, `acos`, `atan` (one and two arguments),
  `atan2`, `exp`, `log` and the `^` operator are correctly rounded and
  computed in-crate rather than by the C library, so the last bit can differ
  from reference Lua, whose results come from the host libm. See
  `determinism.md`.
- `math.log(x, base)` uses Lua 5.2's general-base calculation: `log10(x)` for
  base 10, otherwise the quotient of two correctly rounded natural logarithms,
  which is not itself a correctly rounded `log_base`. The last bit can differ
  from Lua 5.4 (for example `math.log(3, 2)`, where 5.4 uses `log2`).
- `math.random` uses the in-crate seeded generator, so its stream differs from
  reference Lua's.
- Table keys that are integers stop being distinct beyond +/-2^53. See
  "Table library" below.

## Coercion

String-to-number coercion is not implicit in:

- arithmetic;
- numeric `for` control expressions;
- numeric library arguments: `math.*`, `select`, string indices, table
  positions, `tonumber`'s base, `math.random`.

`string.format`'s numeric conversions accept numeral strings, as reference Lua
does.

## Library arguments

- Integer arguments must be numbers with an exact integer value that fits a
  64-bit signed integer. That covers string and table positions, `gsub`'s
  count, `select`'s index, `error`'s level, `tonumber`'s base,
  `math.random`'s bounds and `string.format`'s integer conversions. Anything
  else, `1.5` or `2^63` alike, raises `bad argument #n to 'f' (number has no
  integer representation)`, as in Lua 5.4; Lua 5.2 truncates `1.5` to `1`. A
  numeral string is a type error (see "Coercion"), except in
  `string.format`.
- `string.gsub(s, pattern, repl, n)` with a negative `n` replaces nothing and
  returns `s` and `0`, as with `n = 0` and as in Lua 5.4. Lua 5.2 treats a
  negative `n` as no limit. An absent or nil `n` is unlimited.
- `string.gmatch(s, pattern, init)` accepts Lua 5.4's optional `init`, with
  the same position rules as `string.find`. Lua 5.2 has no `init` and
  ignores a third argument.

## Control flow and syntax

- A numeric `for` with a zero step skips its body in both directions. Lua 5.2
  skips an ascending range but loops forever on a descending one. Lua 5.4
  raises an error.
- Table constructors accept leading and lone comma or semicolon separators,
  for example `{,}` and `{;}`. Lua 5.2 and 5.4 reject those forms.
- Errors kill the callback. There is no `pcall`, `xpcall` or `assert`.

## Errors

- `error(v, level)` adds the `chunk:line:` position only when `v` is a
  string, as Lua 5.4 does; Lua 5.2 also adds it to a number, so `error(42)`
  renders `42` where 5.2 renders `chunk:1: 42`. The level is validated for
  every value, as in both references.
- A table or function passed to `error()` is rendered when it is raised: its
  `__tostring` runs then, before the level argument is validated, so an error
  inside that handler (or its cost) wins over a bad level. Reference Lua keeps
  the value itself and renders it only if the host does. A value without
  `__tostring` renders as `tostring` would (`table: 0x1`, `nil`, `true`),
  where the reference interpreter prints `(error object is a table value)`.
  The host gets the typed value either way (`ErrorValue`).
- Syntax errors carry a column, `chunk:line:column:`.
- When the parser reaches the end of the source where it needed a token, the
  error is `unexpected <eof>` (`SyntaxError::UnexpectedEof`, which
  `Error::is_recoverable` reports), where reference Lua names what it
  expected, for example `'end' expected near <eof>`. Errors the lexer raises
  at the end of the source keep their own kind (`unfinished string`,
  `malformed number`, `unfinished long comment`).
- Lexer errors (`malformed number`, `unfinished string`, invalid escapes)
  name no `near` token, and a missing closing bracket on a later line does
  not add reference Lua's `(to close '(' at line N)`. The parser's other
  errors (`unexpected symbol`, `syntax error`, `'X' expected`, `'=' or 'in'
  expected`, `function arguments expected`, each `near` the offending token)
  use the reference wording.

## Globals and metatables

- Global `unpack` exists, as in Lua 5.2, alongside `table.unpack`.
- `getmetatable(_G)` returns `false`, not `nil`. `_G` is a proxy over the
  global table and its metatable is protected: `setmetatable(_G, ...)` raises
  "cannot change a protected metatable".
- All strings share one metatable, as in reference Lua.
  - `getmetatable("")` returns it.
  - Its `__index` is the string library by default.
  - Method calls such as `s:upper()` resolve through the metatable's current
    `__index`, not through the global `string`. Rebinding or clearing the
    global does not affect them, and they keep working in
    `with_restricted_env` sandboxes.
  - A `__tostring` set on it applies to `tostring(s)`, `print(s)`,
    `string.format("%s", s)` and the host's `tostring_bytes` and
    `to_string_with_meta`, but never to a string passed to `error()`, which
    is the error value itself, as in reference Lua. A
    `__call` makes strings callable as `s(...)`; a non-function `__call`
    chains as in Lua 5.4 (5.2 rejects it).
  - `setmetatable` on a string is an error.
  - Snapshots save the metatable as an environment object (token
    `string.metatable`). See `save-state.md`.
- Arithmetic, comparison and concat metamethods are not implemented. Index,
  newindex, call, len, tostring and metatable-protection fields are. Chains of index, newindex and
  non-function call steps are bounded by `limits::MAX_METAMETHOD_DEPTH`.

## Character classes and locale

- Pattern classes (`%a`, `%l`, `%s`, `%w`, ...) and number-parsing whitespace
  are fixed ASCII, the same on every host. Reference Lua follows `setlocale`,
  so under a Latin-1 or similar locale it can classify bytes 0x80..0xFF as
  letters where dellingr never does. Compare against reference under an
  ASCII-only locale such as `C`.

## Function identity

- Host (Rust) functions are equal when they share a code address, so `f == g`
  between two host functions whose code the linker folded (identical-code
  folding, typical of release LTO builds) can differ from a build that does not
  fold them. Aliases registered under several ids are equal by design.
  Embedders that need distinct identity keep function bodies observably
  distinct.

## Table library

- `table.insert`, `remove`, `concat`, `unpack` (global `unpack` too) and
  `move` honour `__len`, `__index` and `__newindex` as in Lua 5.4. Library
  writes share the VM's metamethod depth limit.
- `table.sort` also honours them, but reads every element first, sorts a
  scratch copy, then writes everything back. Reference Lua interleaves
  handler reads and writes during its quicksort, so a handler that observes
  call order sees a different sequence. Stable handlers and a consistent
  comparator give the same sorted values. Tie order is unspecified.
- Integer keys are f64, and beyond +/-2^53 consecutive integers stop being
  distinct keys. The functions that walk a range of keys therefore refuse a
  range of two or more keys that reaches outside -2^53..2^53 (inclusive),
  raising `<function> range exceeds the exactly representable integer keys`
  instead of reading or writing keys that round onto each other. That
  applies to `table.concat` and `unpack` (global or `table.`) over `i..j`,
  to `table.move`'s source and destination ranges, and to the shifted range
  of a metamethod-driven `table.insert` or `table.remove`. A range of one key
  passes this check wherever it lies, as long as the key itself is an exact
  f64 (an append one past a huge `__len` can fail it); an empty range never reaches it,
  and the functions' other argument checks still apply first. Lua 5.4 has
  integer keys and walks such ranges; for `unpack` it returns the missing
  elements as nils.
- `table.concat` reports a non-string, non-number element with its type and
  index, in the reference wording.
- Field misses on plain tables return nil. There is no implicit table-library
  lookup and no `t:insert(...)` sugar.

## Cost-related differences

These have no reference-Lua analogue. See `budget.md`.

- `while true do end` is free.
- Native string, pattern and table work is charged per byte or element.
