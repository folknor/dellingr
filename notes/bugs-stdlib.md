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

The third resolution wave left a comment at the wrapper and found that the fix
belongs to the VM's error-location code, not to string.rs: recognise the
wrapper by bytecode identity (matching the source name alone would let a user
chunk of the same name be mistaken for it), leave that implementation frame out
of the primary location, and use the first script caller's chunk and call-site
line.

**Fix.** Locate errors raised through the wrapper at the caller's frame, or
remove the wrapper. A RustFunc that can carry an upvalue or a bound argument
would remove the process-global chunk, the `call_unguarded` native level in
`push_gmatch_wrapper`, the snapshot bytecode for the wrapper, and this location
quirk at once. The hunter considers that structural improvement worth having on
its own merits.

## LIB-017 - `_G` proxy reads and writes allocate a `String` per access

From the `stdlib` hunter, raised as a performance observation outside the
defect bar.

`utf8_global_key` allocates a `String` (`name.to_owned()`) on every `_G.k` read
or write that reaches the proxy handlers. The third resolution wave found the
allocation forced by borrowing: the key bytes are borrowed from `State`, while
`get_global` and `set_global` need `&mut State`. Removing it needs a `State` API
that does the global lookup or update from the key's existing stack slot with
the borrowed UTF-8 name, then handles the stack. Tied to LIB-001's deferred
`_G` redesign, which may remove the proxy handlers altogether.
