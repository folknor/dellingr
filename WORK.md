# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #54 and #58: version docs and the assorted-divergence list

Mostly small. The value of this loop is **measurement**: #58 is a list of
unverified claims, and two of them are already disproved below. Do not
implement a claim without checking it first.

### #54 - version and MSRV mismatches

- `Cargo.toml:5` says `rust-version = "1.97"`.
- README badge (line 5) and `AGENTS.md:41` both say **1.92**.
- README's usage snippet says `dellingr = "0.2"`; the crate is **0.3.0**
  (`Cargo.toml:3`).

One MSRV figure is wrong. The local toolchain is 1.99.0-nightly, which does not
settle it. Determine the true minimum - the crate uses edition 2024 and
let-chains, which constrain it - and make all three agree.

### #58 - measured, with two claims disproved

| claim | measured |
|---|---|
| `math.random(m, n)` with `m > n` reports arg `#1`, reference says `#2` | **Invalid.** 5.4 says `#1`; only 5.2 says `#2`. dellingr says `#1` and so matches 5.4. |
| `string.format("%u")` accepted, but 5.4 removed `%u` | **Invalid.** Both 5.2 and 5.4 accept `%u` and print `42`. |
| `math.log(x, base)` is always `ln/ln` | **Real.** `math.log(1000, 10)` is `2.9999999999999996` here, `3.0` in reference. Note `math.log(8, 2)` is already exactly `3` in both, so only some bases diverge. |
| String library rejects number arguments | **Real.** `string.len(42)` errors here; both references return `2`. Same for `string.sub(123, 1)`. |

Still to verify (do not assume):

- `string.format("%p")` printing `(null)` where glibc prints `(nil)`.
- `_G` proxy stringifying keys, so `_G[1]` aliases `_G["1"]`.
- `Frame::jump` (`frame.rs:83-99`) accepting `ip == code.len()`, where the next
  `get_instr` would panic on the out-of-bounds fetch. Unreachable from
  compiler-emitted bytecode since every chunk ends with `OP_RETURN`, but the
  bound should be `<` for defence in depth - and **saved bytecode is
  attacker-controlled**, so confirm whether the snapshot verifier already
  rejects a jump to `code.len()` or whether this is reachable that way.
- Numeric `for` with step 0 skipping the loop (`eval_control.rs:316-326`):
  matches 5.2 for ascending ranges, diverges from 5.2's infinite loop for
  descending ones and from 5.4's `'for' step is zero` error.
- Arithmetic not coercing numeric strings (`"10" + 1` errors; reference gives
  11), while concat *does* coerce numbers to strings.
- `error(msg, level)` ignoring `level`. The position-prefix half is already
  fixed, so errors render `chunk:line: message`; what remains is that
  `error(msg, 2)` cannot blame the caller and `error(msg, 0)` cannot suppress
  the prefix.

### The judgement this loop has to make

Each surviving item is either **a bug to fix** or **a deliberate design choice
to document**. Sort them explicitly; do not fix everything reflexively and do
not document away a real divergence.

The project has a stated "Won't implement" list, and strictness that reads as
deliberate belongs there rather than in a bug tracker. But "it is currently
like this" is not the same as "it was decided". Where the code reads as an
accident, treat it as a bug.

### Constraints

- Determinism unaffected; charge nothing new (#16).
- `unwrap_used` denied outside `#[cfg(test)]`; clippy denies warnings.
- Any behaviour change needs an `examples/*.lua` case or a Rust test.
- Changing string-library coercion affects a lot of call sites - if adopted, it
  must be systematic, not per-function.

---

## Agreed implementation plan

**A third claim is invalid.** `%p` printing `(null)` is *correct*: 5.4
explicitly substitutes `(null)` for a null pointer, and 5.2 rejects `%p`
outright. `string_format.rs` already declares 5.4 compatibility. No change, no
documentation needed.

So of #58's original E-list, three claims were wrong: `math.random`'s argument
number, `%u` removal, and `%p`.

### Fix

**Version metadata.** The real source floor is **1.88** - let-chains
(`error.rs:272`) and `slice::as_chunks` (`object.rs:576`), both stabilized
there, with let-chains needing edition 2024. Nothing in the locked dependency
tree requires 1.97; that manifest change arrived alongside the `hotpath` bump
and did not follow from it. Set `rust-version = "1.92"` (the conservative
supported figure already in README and AGENTS.md) and change the README
dependency snippet to `dellingr = "0.3"`. Only nightly 1.99 is installed here,
so **do not add a build gate that cannot run** - note the need for a real 1.92
check instead.

**`math.log` base 10 only.** dellingr calls `x.log(base)` unconditionally
(`math.rs:287`). 5.2 uses `log10` for base 10; 5.4 adds `log2` for base 2. Use
`log10()` when `base == 10.0` so `math.log(1000,10)` is exactly 3.

**Leave base 2 alone.** It is a genuine 5.2/5.4 split: `math.log(3,2)` gives
`1.5849625007211563` in 5.2 *and dellingr*, `1.5849625007211561` in 5.4.
Changing it trades 5.2 compatibility for 5.4. Record the choice.

**String-library number coercion, wholesale.** Every operand starts with an
exact `LuaType::String` check (`string.rs:242`, `267`, `354`); both references
use `luaL_checklstring`, which accepts numbers and rejects booleans, tables and
functions. The current split is accidental - concat already coerces numbers
(`eval.rs:237`) and `string.format` already accepts a numeric format string
(`string_format.rs:50`). Add **one** shared string-or-number helper and apply
it to every subject and pattern operand in `sub`, `find`, `len`, `upper`,
`lower`, `reverse`, `match`, `gmatch`, `gsub`.

**Do not call `bytes_coerce` without a type gate** - its default conversion
accepts more than strings and numbers (`table_ops.rs:457`).

**`_G` key identity.** `_G[1]` currently aliases `_G["1"]` because non-string
keys go through `to_string`. Keep string keys routed through `State::globals`,
but store non-string keys raw in the proxy table in `global_env_index` /
`global_env_newindex`. `_G` is already captured as an environment object
(`vm.rs:296`) and environment deltas preserve typed keys (`save_state.rs:457`),
so snapshot round-tripping should hold - test it.

**`Frame::jump` bound.** `jump` permits `ip == code.len()` (`frame.rs:100`)
while the next fetch indexes directly (`119`). **Not reachable through a forged
snapshot** - the verifier requires targets strictly less than `code_len`
(`compiler/verify.rs:412`), snapshot loading verifies before materialization,
and there is already a fixture proving rejection (`save_state.rs:1939`). Change
`<=` to `<` as defence in depth and add a direct unit test; do not describe it
as a security fix.

**`error(msg, level)`.** Both references honour it: 0 suppresses the prefix,
N selects the caller N-1 frames up. Parse optional argument 2 defaulting to 1
(`basic.rs:157`), carry a prefix-location policy on the error, and leave the
full traceback unchanged - `Display` currently always takes
`stack_trace.first()` (`error.rs:265`) while the trace already holds the
current frame followed by its callers (`vm.rs:818`). Test default, 0, 1, 2,
out-of-range and a non-integer level.

### Document, do not change

**Zero-step numeric `for`** - README "Compatibility divergences" (new section).
dellingr skips both directions; 5.2 skips ascending but loops forever
descending; 5.4 errors. The code explicitly skips to avoid an infinite loop
(`eval_control.rs:319`), so this is a semantic policy, not an omission.

**Numeric-string arithmetic** - README "Won't implement", as: *implicit
string-to-number coercion in arithmetic and numeric control expressions*.
Arithmetic funnels through strict `as_num()` (`eval_store.rs:507`), unary
negation likewise (`190`), and numeric `for` too (`eval_control.rs:62`). That
breadth reads as deliberate VM policy, unlike the isolated string-library
mismatch above - which is why the two are sorted differently.

Also record the base-2 `math.log` 5.2/5.4 split in the same divergences
section.

### Superseded open questions

1. **What is the true MSRV?** Edition 2024 and let-chains set a floor. Which of
   1.92 / 1.97 is right, and does anything in the crate actually require the
   higher one?
2. **String-library number coercion.** Reference coerces numbers to strings
   throughout the string library. Should dellingr adopt that wholesale, or is
   the strictness deliberate and belongs on the "Won't implement" list?
   Consider that concat already coerces numbers, so the current split is
   internally inconsistent. Which way does that inconsistency argue?
3. **`math.log` base special-casing.** 5.4 special-cases bases 2 and 10 to
   return exact results. Is matching that worth it given the README's
   transcendental caveat, and does it affect determinism across hosts?
4. **`for` step 0 and numeric-string arithmetic.** Bug or documented decision?
   Argue each. If documented, say exactly where - README "Won't implement" or a
   divergence-notes section.
5. **`Frame::jump`.** Is `ip == code.len()` reachable through a forged
   snapshot, given the bytecode verifier? If so this is a reachable panic and
   not merely defence in depth.

Read `Cargo.toml`, `README.md`, `AGENTS.md`, `src/lua_std/math.rs`,
`src/lua_std/string.rs`, `src/lua_std/string_format.rs`, `src/vm/frame.rs`
(`jump`), `src/vm/eval_control.rs` (numeric for), `src/vm/eval_store.rs`
(arithmetic coercion), and `src/compiler/verify.rs`.
